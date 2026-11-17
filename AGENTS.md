# Working in this repository

Read the "Development checks" and "Changes and releases" sections of `CONTRIBUTING.md`. For an agent, they come down to:

- Start every piece of work with a GitHub issue, including bug fixes, features, documentation, refactoring and maintenance. Reuse an existing issue only when it already covers the same work. Handle the issue in a dedicated pull request that links to it (`Closes #N`), and keep unrelated work in separate issues and pull requests.
- Work on a branch made from the latest `main` (`git fetch origin && git switch -c <branch> origin/main`), push it, and open a pull request into `main`. Never push to `main`: it accepts changes only through pull requests whose `CI gate` check passed. CI runs in the pull request; locally, run only the checks of what you changed. When `main` moved and your pull request conflicts, rebase it on the new `main` and push again.
- Never choose a version. Do not write `docs/releases/X.Y.Z.md`, bump a version, or create a tag by hand.
- When your change makes operators act before or after upgrading (a Hub/Node or client protocol change, something removed or changed in meaning, manual configuration, an upgrade that cannot be undone by reinstalling the previous release), start its release fragment with `Needs a minor release: <why>`. See "Changes and releases" in `CONTRIBUTING.md`.
- Describe your change for the user in `docs/releases/unreleased/<item>.md` (for example `ST-38.md`), with no version and no title. Run `python3 ops/cut-release.py lint` before delivering.
- When the change should be released and is on `main`, run `python3 ops/cut-release.py cut`. It picks the version, brings the release notes in through a release pull request, merges it once its CI passed and pushes the tag. If it says origin moved, it has already tried again; if it still refuses, report why instead of tagging yourself.
- A new database migration takes the next free number when it is merged, with a line appended to `hub-rs/crates/steve-store/migrations/LOCK`.
- Keep personal paths, hostnames and credentials out of the repository: `python3 scripts/check-public-boundary.py --strict`.

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
