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

CI (`.github/workflows/ci.yml`) runs on every pull request and push to `main`, on Linux and macOS. Web and release-script checks run independently of Rust. Each platform compiles the workspace once with [cargo-nextest](https://nexte.st) 0.9.151, archives the test binaries and their executable dependencies, then runs four `hash:M/4` partitions on separate runners. The source checkout, absolute workspace path, platform and Rust toolchain must match the archive: fixtures and `env!("CARGO_BIN_EXE_...")` paths remain valid. `cargo test --doc` reuses the compilation job's dependencies. CI uses line tables and disables Rust incremental artifacts to keep transfers small; it preserves every assertion and the release profile.

The final `CI gate` requires every job to succeed and compares the complete test inventory against all four actual partition lists and JUnit execution reports on both platforms (`scripts/nextest-partitions.py`). Each ordinary test must be selected exactly once; opt-in ignored tests remain ignored. Every partition uploads its list and JUnit report, and `scripts/nextest-summary.py` writes the test verdicts to the job summary. Automatic retries are disabled (`--retries 0`): a flaky first attempt fails CI and must be investigated. `cut` accepts only the **entire workflow's** completed successful push run on the exact main commit, never an individual successful partition or a pull-request run.

Official GitHub cache actions restore Rust dependencies by platform, toolchain and lockfile. Workspace packages are cleaned both after restoring and before saving a cache, so they are always rebuilt from the current checkout. Pull requests only read caches; trusted main pushes and release tags write them. Tag caches can speed up reruns of that tag; GitHub's cache scope does not let a later tag reuse another tag's cache. The release workflow builds the web assets once alongside native Linux/macOS builds, then packages and verifies each target on its native platform; publication still waits for both complete bundles.

- **No process documents.** Design notes, plans, proposals, specs, reviews, prototypes and runbooks are the maintainers' own thinking and are kept outside the repository. What a user or operator needs goes into the README, `docs/deployment.md` or the release notes; what a reader of the code needs goes into its comments. `python3 scripts/check-public-boundary.py --strict` (run by CI) rejects any other Markdown file, and anything under `docs/` other than `deployment.md`, `releases/` and `assets/`; test fixtures are exempt.

## Changes and releases

A version is chosen when it is released, never ahead on a branch:

- **Release notes are fragments.** A change adds `docs/releases/unreleased/<item>.md` (its item, such as `ST-38.md`, or its branch name): what changes for the user, in a few sentences or bullets, with no version and no title. Do not write `docs/releases/X.Y.Z.md` on a branch; `python3 ops/cut-release.py lint` checks this.
- **Releasing:** after the change is on `main`, run `python3 ops/cut-release.py cut`. It takes `main` as it is on `origin`, gives it the next free version (one more than the highest `vX.Y.Z` tag), joins the fragments into `docs/releases/X.Y.Z.md`, and pushes the commit and the annotated tag together. If someone else releases first, it starts again with the next number. `--dry-run` shows the notes without releasing; `--minor` bumps the minor version.
- **Only a tested `main` is tagged.** `cut` asks GitHub (with `gh`) for the CI run on the commit it is about to release, waits while it runs (`--ci-wait`, 45 minutes by default), and refuses when it failed or there is none. Investigate test failures before releasing; a rerun is appropriate for a resolved runner or service failure, not a substitute for fixing a flaky test. `--skip-ci` releases without the check, on purpose. If the Release build on the tag fails on a flake, rerun it the same way on the same tag; nothing is published until every target is built.
- **Database migrations** take their number when they are merged: the next free one in `hub-rs/crates/steve-store/migrations`, with a line appended to `LOCK`.
