# Working in this repository

Read the "Development checks" and "Changes and releases" sections of `CONTRIBUTING.md`. For an agent, they come down to:

- Never choose a version. Do not write `docs/releases/X.Y.Z.md`, bump a version, or create a tag by hand.
- Describe your change for the user in `docs/releases/unreleased/<item>.md` (for example `ST-38.md`), with no version and no title. Run `python3 ops/cut-release.py lint` before delivering.
- When the change should be released and is on `main`, run `python3 ops/cut-release.py cut`. It picks the version, writes the release notes and pushes `main` with the tag in one step. If it says origin moved, it has already tried again; if it still refuses, report why instead of tagging yourself.
- A new database migration takes the next free number when it is merged, with a line appended to `hub-rs/crates/steve-store/migrations/LOCK`.
- Keep personal paths, hostnames and credentials out of the repository: `python3 scripts/check-public-boundary.py --strict`.

## Open-source portability

- Steve is standalone open-source software. Its design and core mechanisms must work without any contributor's personal infrastructure, private network, accounts, Wiki, machine names, or directory layout.
- Treat an operator's environment as a deployment example, never as the product specification. Derive paths and capabilities from installation configuration and discovery.
- Keep model backends, Git hosting, CI, artifact stores, service managers, and credential stores behind documented contracts. Optional integrations must not become mandatory dependencies of core workflows.
- Ordinary installations consume trusted release artifacts without a source checkout, compiler, or upstream publishing credentials. Source modification and publication belong to an explicitly configured maintainer workflow.
- Keep diagnosis and recovery usable when the Hub or model backend is unavailable. Acceptance must include clean supported installations with no access to a contributor's private services.
- Keep implemented public contracts and operator instructions in the README, `docs/deployment.md`, release notes and code comments, following `CONTRIBUTING.md`. Private design notes must not be required to operate the software. Self-improvement remains a later direction, not a claim of implemented functionality.
