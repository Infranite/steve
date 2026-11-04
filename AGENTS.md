# Working in this repository

Read the "Development checks" and "Changes and releases" sections of `README.md`. For an agent, they come down to:

- Never choose a version. Do not write `docs/releases/X.Y.Z.md`, bump a version, or create a tag by hand.
- Describe your change for the user in `docs/releases/unreleased/<item>.md` (for example `ST-38.md`), with no version and no title. Run `python3 ops/cut-release.py lint` before delivering.
- When the change should be released and is on `main`, run `python3 ops/cut-release.py cut`. It picks the version, writes the release notes and pushes `main` with the tag in one step. If it says origin moved, it has already tried again; if it still refuses, report why instead of tagging yourself.
- A new database migration takes the next free number when it is merged, with a line appended to `hub-rs/crates/steve-store/migrations/LOCK`.
- Keep personal paths, hostnames and credentials out of the repository: `python3 scripts/check-public-boundary.py --strict`.
