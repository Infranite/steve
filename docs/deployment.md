# Deploying and upgrading Steve

[README](../README.md) gets a local instance running in a few minutes. This page covers running Steve as a long-lived service: a source-based macOS installation, managed Release bundles with automatic upgrades, and how an upgrade behaves.

- [Source-based macOS service installation](#source-based-macos-service-installation)
- [Managed Release bundles and automatic upgrades](#managed-release-bundles-and-automatic-upgrades)
- [Pairing a separate Node](#pairing-a-separate-node)

## Pairing a separate Node

Remote Nodes are experimental. They run `steve-node`, a separate binary from `steve-hub`. Current Release bundles contain the Hub and Web, so build the Node from a compatible source revision on the target OS and architecture:

```sh
cargo build --locked --release --manifest-path hub-rs/Cargo.toml \
  -p steve-node-cli --bin steve-node
```

The binary is `hub-rs/target/release/steve-node` (or under `CARGO_TARGET_DIR` when set). Record its source revision and SHA256 before copying it to the target machine. Keep it in a stable location independent of the Hub's release directory.

Before pairing, verify that the Hub has `STEVE_NODE_LISTEN=1` and that its HTTPS reverse proxy forwards `/node/pair` and WebSocket upgrades at `/node/ws` to the Hub over loopback. The Node listener rejects non-loopback TCP peers; exposing the Hub's HTTP port directly to another machine is not sufficient. Back up existing configuration before enabling the listener, and apply it through the installation's normal restart procedure.

Give the Node its own data directory. Set `STEVE_HOME` explicitly for every Node invocation and in its service environment; never inherit the Hub's data directory. The default, when unset, is `~/.steve-node`. Preserve an existing Node's `node/identity.key`, `node/node.json` and `node/local/` journal when updating it. Do not re-pair an existing identity as a way to reconnect.

Pairing and confirmation currently use the owner's authenticated Hub API session; there is no device pairing page yet. Agent bearer tokens do not replace that session. API writes also require the selected workspace's `x-steve-ws` header.

1. As the signed-in owner, call `POST /api/nodes/pairings`. Keep the returned `code` private: it expires after ten minutes and can be used once.
2. On the Node, run the following command and enter the code at its stdin prompt. The code is **not** a positional argument. With a publicly trusted HTTPS certificate, no `--pin` is needed. For a pinned certificate, add `--pin sha256:<verified-certificate-DER-hash>`, obtained through a trusted channel.

   ```sh
   STEVE_HOME="<absolute-node-data-directory>" \
     <absolute-path-to-steve-node> pair "https://<hub>" --name "<node-name>"
   ```

3. Call `GET /api/nodes` as the same owner. Save the pending Node's `node_id` from this list and match its fingerprint with the CLI output, then call `POST /api/nodes/<node_id>/confirm` and check for `{"ok": true}`. Use that Node ID for subsequent checks; do not identify it by name alone.
4. Run the Node in the foreground, or configure its service manager to run this command with absolute paths:

   ```sh
   STEVE_HOME="<absolute-node-data-directory>" <absolute-path-to-steve-node> run
   ```

On macOS, use a dedicated user LaunchAgent with `RunAtLoad`, `KeepAlive`, an explicit `STEVE_HOME` and `PATH`, and separate stdout/stderr logs. This starts in the user's login session. Do not attach it to the Hub's service or restart all Node processes during deployment. On Linux, use a separate foreground systemd service with `Restart=on-failure`.

The Node can register with no `roots.json`; in that case it exposes no project roots. To enable execution later, configure only the intended directories in `roots.json` and the installed ACP adapters and Node.js executable in `harness.json`. Adapter paths should be absolute because a service's `PATH` can differ from an interactive shell. These files live directly under the Node's data directory:

```json
{
  "adapters": {"<harness-id>": "/absolute/path/to/acp-adapter"},
  "node": "/absolute/path/to/node"
}
```

Registration alone does not move existing projects or change scheduling. Do not set `STEVE_NODE_BIND` merely to register a Node. The current remote Node configuration advertises session capacity; jobs and execs have zero capacity.

### Verify and recover a Node

`steve-node status` reports the saved pairing and fingerprint; it does not establish that the process is online. Verify `GET /api/nodes` reports the same Node ID with `state: "active"` and `linked: true`, and that its own log contains `linked to the hub`. After restarting only the new service, verify the same identity reconnects. Verify execution separately with a bounded test session when an ACP adapter and a project root are configured.

If deployment fails, stop only the newly installed service. Preserve its logs, identity and journal for diagnosis and retry. If abandoning a confirmed registration, the owner can call `POST /api/nodes/<node_id>/revoke`; a revoked identity cannot reconnect. The local `forget` command only removes pairing files and does not revoke access at the Hub. Restore any backed-up Hub configuration through the same controlled restart procedure if enabling the listener must be rolled back.

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
