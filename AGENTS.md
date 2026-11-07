# Working in this repository

Read the "Development checks" and "Changes and releases" sections of `CONTRIBUTING.md`. For an agent, they come down to:

- Never choose a version. Do not write `docs/releases/X.Y.Z.md`, bump a version, or create a tag by hand.
- Describe your change for the user in `docs/releases/unreleased/<item>.md` (for example `ST-38.md`), with no version and no title. Run `python3 ops/cut-release.py lint` before delivering.
- When the change should be released and is on `main`, run `python3 ops/cut-release.py cut`. It picks the version, writes the release notes and pushes `main` with the tag in one step. If it says origin moved, it has already tried again; if it still refuses, report why instead of tagging yourself.
- A new database migration takes the next free number when it is merged, with a line appended to `hub-rs/crates/steve-store/migrations/LOCK`.
- Keep personal paths, hostnames and credentials out of the repository: `python3 scripts/check-public-boundary.py --strict`.
- No process documents in the repository: design notes, plans, proposals, specs, reviews, prototypes and runbooks stay out of it, and so do edits to bring one back. The only documents are the READMEs, `CONTRIBUTING.md`, `AGENTS.md`, `docs/deployment.md` (what an operator does) and the release notes; explain code in its comments. `check-public-boundary.py` rejects any other Markdown file or anything else under `docs/`.
