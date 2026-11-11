# Contributing to Steve

Thanks for helping. Build and run Steve locally as described in the [README](README.md#quick-start), then use the checks and release rules below.

## Development checks

```sh
python3 scripts/test-public-boundary.py
python3 scripts/check-public-boundary.py --strict
python3 ops/test-cut-release.py
cargo test --locked --manifest-path hub-rs/Cargo.toml -p steve-install
python3 packaging/homebrew/test-sync.py
python3 ops/cut-release.py lint
cargo test --locked --manifest-path hub-rs/Cargo.toml --workspace
npm run check
npm run test:web
```

The workspace suite includes the Node boundary guard (`hub-rs/crates/steve-hub/tests/node_boundary.rs`): production code outside `steve-node` may start processes or harnesses, list directories, or open, write, create or remove files and directories, only at the call sites its table allows, each with a reason; a new one fails until it goes through the Node or is listed. Imports count too, so a braced, renamed or glob `use` of a listed item cannot hide a call. It is a text scan, so it sees where harness handles are made and imported, not every place one is passed to.

The harness parity guard (`hub-rs/crates/steve-hub/tests/harness_parity.rs`, and `web/test/harness-parity.test.mjs` for the web) fails when production code tells harnesses apart by name outside the adapter layer (`steve-acp/src/config.rs`, `steve-acp/src/adapters.rs`). Declare a property in the adapter registry instead. Places that still do are listed in `harness_parity.txt`; the list may only shrink, and `STEVE_PARITY_BLESS=1` rewrites it after removing one.

The full Rust test suite also requires `zsh` at `/bin/zsh` for the existing source-release fixtures (on Debian/Ubuntu, install the `zsh` package). The boundary scripts require Python 3.11+. macOS keychain integration tests are explicitly opt-in and create temporary signing resources; ordinary development must not use production credentials. The TypeScript `hub/` workspace is retained for compatibility; the default managed backend is Rust.

To try a build by hand with a real harness, use a [second installation](docs/deployment.md#a-second-installation-under-the-same-user): its own `--home`, `--label`, `--listen` port and `--vault-dir`, and, before its first start, [its harnesses' own state directories](docs/deployment.md#its-harnesses-own-state). Without the last, the trial's main conversation keeps its history and memory in your own `~/.claude` or `~/.codex`.

CI (`.github/workflows/ci.yml`) runs on every pull request and push to `main`, on Linux and macOS. Web and release-script checks run independently of Rust. One macOS job runs the Swift package's tests (`scripts/build-apple-client.sh test`) and builds the macOS app and the iOS app for the simulator, unsigned, around the web client of the same commit. Each platform has one Rust job: it compiles the workspace's tests with [cargo-nextest](https://nexte.st) 0.9.151, runs the whole suite on the same runner with each test in its own process, and then runs the doctests from the same build. Nothing is handed between runners: tests run from the checkout and target directory they were compiled in. CI uses line tables and disables Rust incremental artifacts; it preserves every assertion and the release profile.

After the tests, the job compares the complete test inventory with the run's JUnit execution report (`scripts/nextest-partitions.py`): each ordinary test must have run exactly once and passed on its first try; opt-in ignored tests remain ignored. The job uploads the inventory, the report and Cargo's build timings (`cargo-timing.html`: how long each crate and test binary took to compile), and `scripts/nextest-summary.py` writes the test verdicts to the job summary. Automatic retries are disabled (`--retries 0`): a flaky first attempt fails CI and must be investigated. The final `CI gate` requires every job on both platforms to succeed. Every job in both workflows has a time limit (`timeout-minutes`) of two to several times its longest observed run: a job that hangs fails there instead of holding a runner for GitHub's default of six hours.

A push to `main` that changed only `docs/releases/` (the release notes commit `cut` pushes) does not repeat the Rust jobs or the Apple job: nothing they build or test reads that directory. `scripts/skip-rust-jobs.py` waits for this workflow's run on the commit before the push and skips them when that run passed, so a passed run always stands on Rust jobs that ran on the same code. They run when that run did not pass, when any other file changed, or when GitHub could not say; pull requests always run them. The other jobs and the gate run on every push.

`cut` accepts only the **entire workflow's** completed successful run, never an individual successful job: the push run on the exact main commit, or the run of the pull request that commit was merged from when it tested the same tree (see "Only a tested `main` is tagged" below).

Official GitHub cache actions restore Rust dependencies under a key made of the platform, the toolchain, the lockfile, every manifest and the workflow file: everything that decides how dependencies are built. Workspace packages are cleaned both after restoring and before saving a cache, so they are always rebuilt from the current checkout. Pull requests only read caches, and one whose key has no cache yet starts from `main`'s newest cache for its platform; only trusted main pushes write them. A push to `main` saves a cache only when its key has none, built from nothing, so there is one cache per dependency set rather than one per push. The release build uses no cache and always compiles from nothing: GitHub's cache scope does not let a tag read another tag's cache, so a cache saved there was never read. The release workflow builds the web assets once alongside native Linux/macOS builds, then packages and verifies each target on its native platform; publication still waits for both complete bundles.

- **No process documents.** Design notes, plans, proposals, specs, reviews, prototypes and runbooks are the maintainers' own thinking and are kept outside the repository. What a user or operator needs goes into the README, `docs/deployment.md` or the release notes; what a reader of the code needs goes into its comments. `python3 scripts/check-public-boundary.py --strict` (run by CI) rejects any other Markdown file, and anything under `docs/` other than `deployment.md`, `releases/` and `assets/`; test fixtures are exempt.

## Changes and releases

A version is chosen when it is released, never ahead on a branch:

- **Release notes are fragments.** A change adds `docs/releases/unreleased/<item>.md` (its item, such as `ST-38.md`, or its branch name): what changes for the user, in a few sentences or bullets, with no version and no title. Do not write `docs/releases/X.Y.Z.md` on a branch; `python3 ops/cut-release.py lint` checks this.
- **Releasing:** after the change is on `main`, run `python3 ops/cut-release.py cut`. It takes `main` as it is on `origin`, gives it the next free version (one more than the highest `vX.Y.Z` tag), joins the fragments into `docs/releases/X.Y.Z.md`, and pushes the commit and the annotated tag together. If someone else releases first, it starts again with the next number. `--dry-run` shows the notes without releasing; `--minor` bumps the minor version.
- **Only a tested `main` is tagged.** `cut` asks GitHub (with `gh`) for the CI run on the commit it is about to release, waits while it runs (`--ci-wait`, 45 minutes by default), and refuses when it failed or there is none. Until that run has finished, the pull request's own run counts when it tested exactly what is being released: the pull request was up to date with `main` and rebase-merged, so `main` now ends in the same commits with the same trees, one for one. `cut` checks this itself, and then releases without waiting for the second run; a pull request that was behind `main`, was squashed, or whose run did not pass still waits for the run on `main`. Investigate test failures before releasing; a rerun is appropriate for a resolved runner or service failure, not a substitute for fixing a flaky test. `--skip-ci` releases without the check, on purpose. If the Release build on the tag fails on a flake, rerun it the same way on the same tag; nothing is published until every target is built.
- **Database migrations** take their number when they are merged: the next free one in `hub-rs/crates/steve-store/migrations`, with a line appended to `LOCK`.
