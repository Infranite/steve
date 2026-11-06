# Steve

Steve is a self-hosted assistant workspace: conversations, tasks, knowledge, integrations and tools share a Rust Hub and a React web interface. Source code is MIT licensed; see [LICENSE](LICENSE). External agents and providers require your own installation and configuration.

```text
Browser -> React UI -> Rust Hub -> SQLite / local files
                           +-- Agent processes
                           +-- Optional integrations
                           +-- Credential vault
```

## Build and run locally

Prerequisites: Git, current stable Rust/Cargo, Node.js 24 LTS and npm. macOS source-based service installation additionally needs zsh, Python 3, sqlite3 and Xcode command-line tools. Provider CLIs are optional until you use that provider.

```sh
git clone https://github.com/Infranite/steve.git
cd steve
npm ci
npm run build:web
cargo build --locked --manifest-path hub-rs/Cargo.toml -p steve-hub
STEVE_HOST=127.0.0.1 STEVE_PORT=8440 STEVE_WEB_DIR="$PWD/web/dist" \
  hub-rs/target/debug/steve-hub
```

Open `http://127.0.0.1:8440`. Runtime data defaults to `~/.steve`; set `STEVE_HOME` to isolate a test instance. A public domain is optional for local use. Configure your own reachable URL when an integration needs callbacks; no shared hosted endpoint is supplied. Review authentication before exposing the listener beyond localhost.

## Source-based macOS service installation

Copy `ops/steve.env.example` to `~/.steve/steve.env` and adjust it to your deployment. Keep secrets outside this checkout, using the supported `*_FILE` configuration. On macOS the vault master keys live in the login keychain and only `~/.steve/bin/steve-keyhelper` reads or writes them; `ops/release.sh` rebuilds it only when its own source changes, so rebuilt Hubs keep access to the keys. Run `zsh ops/install.sh` from the checkout to build a source release and install user launchd jobs. Logs are under `~/.steve/logs`.

`ops/release.sh` builds from `main` and switches source releases after checks. `STEVE_REPO`, `STEVE_HOME`, `STEVE_LABEL` and `STEVE_BACKUP_LABEL` support custom locations and service labels. Existing installations must preserve their original signing identities and keychain service: set `STEVE_KEYHELPER_SERVICE`, `STEVE_KEYHELPER_HUB_IDENTIFIER`, `STEVE_KEYHELPER_IDENTIFIER` and `STEVE_LABEL` to the original values in the build environment (they are compile-time identities, not `steve.env` settings) and sign with the original certificate. Changing a namespace or vault backend does not migrate keys. Back up data and verify recovery material before changes.

This source-based service can check GitHub Releases; it does not use the bundle supervisor and cannot execute automatic Release upgrades. Keep using your source deployment process unless you explicitly migrate to the managed bundle layout below.

## Managed Release bundles and automatic upgrades

Automatic upgrades require a published bundle matching your platform and a service running Steve's supervisor. Release builds target Apple Silicon macOS 14+ and Linux x86_64 with glibc 2.35+ (Ubuntu 22.04+ or Debian 12+); release-asset availability determines which platform can actually upgrade. Windows is not supported. Source development and externally managed containers expose checks only.

Initial installation is a manual deployment step:

1. Select a stable release from [Infranite/steve Releases](https://github.com/Infranite/steve/releases) with a bundle for your OS and architecture. Verify its manifest and SHA256 before extracting it. If no matching release asset has been published, this installation path is not available yet.
2. Install the extracted, paired Hub/Web bundle under `$STEVE_HOME/releases/<version>` and point `$STEVE_HOME/current` at that directory. A bundle contains `bin/steve-hub`, `web/dist`, and `release.json`; do not substitute a source checkout or mix separate frontend/backend builds.
3. Configure `$STEVE_HOME/steve.env`, authentication, the optional public URL, and a supported persistent vault backend. New installations can explicitly use `STEVE_VAULT_KEYSTORE=dir` with `STEVE_VAULT_KEY_DIR` outside `STEVE_HOME`, on protected storage owned by the Hub user with mode 0700, kept mounted across releases. The `file` backend is for isolated development and tests only. Existing macOS Keychain/helper installations cannot use generic precompiled upgrades because those binaries do not preserve their private signing identity; plan a separate, validated migration if needed.
4. Configure your service manager to run the absolute path to `ops/run-managed.sh` in the foreground. This launcher starts `steve-hub --supervise`; it does not download or install the initial bundle or create a service-manager job. For systemd use `Type=simple`, `Restart=on-failure`, `KillMode=control-group` and `TimeoutStopSec=600` (more if database backups take longer); for launchd use a foreground `ProgramArguments` entry and `KeepAlive`. Do not wrap the script in anything that discards the supervisor PID.

```text
Service manager
      |
      v
steve-hub --supervise
      |
      +-- current/bin/steve-hub -> current/web/dist
      |
      +-- updates journal + verified database backups
```

In Settings -> About -> Versions, check for updates and review the release notes. Only the instance owner can execute an upgrade or approve/reject an Agent request. Agents use `steve_upgrade` to check status. Upgrade requests are available only in a workspace whose sole member is the instance owner; shared-workspace agents cannot request instance upgrades. Workspace ownership alone does not grant upgrade permission.

An upgrade downloads and validates the bundle, waits for active work to drain, stops the old Hub, backs up SQLite, then verifies the candidate before committing the version switch. Failures before commit restore the old bundle and database; failures after commit retain the new database to avoid discarding resumed work or external effects. The UI reconnects without submitting the upgrade again. Completed upgrades leave the current page open until you choose to reload; persisted conversation drafts are retained.

The Hub checks for a newer release shortly after it starts and then every six hours (`STEVE_UPDATE_CHECK=0` turns this off; debug builds only check with `STEVE_UPDATE_CHECK=1`). When it finds one, the sidebar and Settings show a notice, and the owner sees it once as a toast. Only the owner can start a check from the page.

A macOS install built locally with `ops/local-release.py` (signed Hub, Keychain vault) also upgrades from this page. It needs `signing/identity.json`, the signing keychain and `keychain-pass`, a checkout at `STEVE_REPO`, and `cargo` (or `STEVE_CARGO`), `npm`, `git` and Python 3.12+ (`STEVE_PYTHON`, or the first of `python3.13`, `python3.12`, `python3` on `PATH` or in Homebrew's bin that is new enough). The page lists anything missing. The Hub resolves the release from `STEVE_UPDATE_REPOSITORY` and accepts only a stable version newer than the running one. It then starts a one-shot launchd job (`<label>.upgrade`) with a minimal environment. The job fetches the tag, refuses it when it doesn't point at the commit in the release manifest, and runs that tag's own `local-release.py build` and `deploy --operation <id>`. Each step goes into the same journal, so the page follows it across the restart. Its log is `$STEVE_HOME/logs/release/upgrade-<version>-<id>.log`. Only one release runs at a time: `ops/release.sh`, `local-release.py` and the job all share `releases/.release.lock`.

Eligible interrupted work resumes through task recovery. Paused work and pending approvals remain waiting; uncertain external delivery outcomes require reconciliation. Keep `$STEVE_HOME/updates` journals, backups and old versions until you accept the upgrade. The initial implementation does not automatically prune them.


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

## Changes and releases

A version is chosen when it is released, never ahead on a branch:

- **Release notes are fragments.** A change adds `docs/releases/unreleased/<item>.md` (its item, such as `ST-38.md`, or its branch name): what changes for the user, in a few sentences or bullets, with no version and no title. Do not write `docs/releases/X.Y.Z.md` on a branch; `python3 ops/cut-release.py lint` checks this.
- **Releasing:** after the change is on `main`, run `python3 ops/cut-release.py cut`. It takes `main` as it is on `origin`, gives it the next free version (one more than the highest `vX.Y.Z` tag), joins the fragments into `docs/releases/X.Y.Z.md`, and pushes the commit and the annotated tag together. If someone else releases first, it starts again with the next number. `--dry-run` shows the notes without releasing; `--minor` bumps the minor version.
- **Database migrations** take their number when they are merged: the next free one in `hub-rs/crates/steve-store/migrations`, with a line appended to `LOCK`.
