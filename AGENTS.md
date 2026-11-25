# Working in this repository

Read the "Development checks" and "Changes and releases" sections of `CONTRIBUTING.md`. For an agent, they come down to:

- Start every piece of work with a GitHub issue, including bug fixes, features, documentation, refactoring and maintenance. Reuse an existing issue only when it already covers the same work. Handle the issue in a dedicated pull request that links to it (`Closes #N`), and keep unrelated work in separate issues and pull requests.
- Work on a branch made from the latest `main` (`git fetch origin && git switch -c <branch> origin/main`), push it, and open a pull request into `main`. Never push to `main`: it accepts changes only through pull requests whose `CI gate` check passed. CI runs in the pull request; locally, run only the checks of what you changed. When `main` moved and your pull request conflicts, rebase it on the new `main` and push again.
- **Commits and PR titles:** use Conventional Commits (`type(scope): imperative summary`, `type` in `feat|fix|perf|refactor|docs|test|build|ci|chore|revert`). Mark breaking public-contract changes with `!` or a `BREAKING CHANGE:` trailer. When operators must act before or after upgrading, add a trailer `Upgrade: <what to do>` (may span multiple lines). Add an optional `## Release note` section in the PR description for user-facing detail; otherwise the PR title is used. Run `python3 ops/cut-release.py lint` before delivering.
- **Never choose a version:** do not write `docs/releases/X.Y.Z.md`, bump a version, or create a tag by hand. `ops/cut-release.py` computes `MAJOR.MINOR.PATCH` and generates release notes from commits since the last tag (MAJOR = public contract generation, `0` until `1.0.0`, bumped post-1.0 only on breaking changes; MINOR = `feat`, `Upgrade:`, or pre-1.0 breaking; PATCH = fixes/perf/docs/refactors only). See "Changes and releases" in `CONTRIBUTING.md`.
- When changes on `main` should be released, run `python3 ops/cut-release.py cut` (use `next` or `cut --dry-run` to preview). If it refuses after retrying, report why instead of tagging yourself.
- A new database migration takes the next free number when it is merged, with a line appended to `hub-rs/crates/steve-store/migrations/LOCK`.
- Keep personal paths, hostnames, credentials, and process documents out of the public repository: `python3 scripts/check-public-boundary.py`.

## Engineering standards

- **No legacy or transition bridges:** go straight to the target design. Do not retain compatibility shims, dual paths, or transitional fallbacks; land large refactors as one coherent cutover rather than half-migrated states.
- **Metrics at every layer boundary:** leave structured metric and telemetry hooks at each boundary (HTTP/RPC, queue, storage, protocol, adapter) so latency, failures, and throughput can be observed without retrofitting.
- **Deterministic first, models as glue, no magic numbers:** handle everything deterministic in code and typed rules; use models only as glue for judgment or synthesis. Name thresholds, timeouts, and limits as explicit constants or configuration with rationale—never bare magic numbers.
- **Strongly typed errors and contracts:** carry typed error enums and explicit data contracts across crates and APIs; do not smuggle untyped strings or ad-hoc JSON maps through business logic.
- **Type-checked tests and fixtures:** every test, mock, and fixture (Rust and TypeScript/browser) must pass the same strict type checks as production code. Keep tests focused on contracts and delete tests that existed only for removed code.
- **Human-first UI:** before designing or modifying a screen, decide in what situation the user opens it and what the first screen must answer; organize around that task rather than mirroring backend tables.

## Open-source portability

- Steve is standalone open-source software. Its design and core mechanisms must work without any contributor's personal infrastructure, private network, accounts, Wiki, machine names, or directory layout.
- Treat an operator's environment as a deployment example, never as the product specification. Derive paths and capabilities from installation configuration and discovery.
- Keep model backends, Git hosting, CI, artifact stores, service managers, and credential stores behind documented contracts. Optional integrations must not become mandatory dependencies of core workflows.
- Ordinary installations consume trusted release artifacts without a source checkout, compiler, or upstream publishing credentials. Source modification and publication belong to an explicitly configured maintainer workflow.
- Keep diagnosis and recovery usable when the Hub or model backend is unavailable. Acceptance must include clean supported installations with no access to a contributor's private services.
- Keep implemented public contracts and operator instructions in the README, `docs/deployment.md`, release notes and code comments, following `CONTRIBUTING.md`. Private design notes must not be required to operate the software. Self-improvement remains a later direction, not a claim of implemented functionality.

## UI components

- affectUI (https://www.affectui.com, catalogue in `llms.txt`) is the first choice for UI. Before building a component, check whether an affectUI item covers it, and use it even where Steve already has its own version. Its `--aui-*` tokens follow Steve's theme through `web/src/styles/affectui-theme.css`.
- Fetch items with `python3 scripts/affectui-pull.py <slug>` into `web/src/components/affect/`. Pro items need the maintainer's key (`AFFECTUI_TOKEN` or `~/.config/affectui/token`). Never commit the key.
- Keep the notice the script writes on line 1 of every affectUI file. Pro files stay under the affectUI license, not Steve's. They may be adapted inside Steve but not published on their own.
- Wire components to real Steve data. Don't add another component library for something affectUI covers.
