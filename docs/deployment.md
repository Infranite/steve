# Deploying and upgrading Steve

[README](../README.md) gets a local instance running in a few minutes. This page covers running Steve as a long-lived service: a source-based macOS installation, managed Release bundles with automatic upgrades, and how an upgrade behaves.

- [Hub and Node service commands](#hub-and-node-service-commands)
- [Source-based macOS service installation](#source-based-macos-service-installation)
- [Managed Release bundles and automatic upgrades](#managed-release-bundles-and-automatic-upgrades)
- [Pairing a separate Node](#pairing-a-separate-node)
- [Remote Nodes](#remote-nodes)
- [Diagnostic capture and queries](#diagnostic-capture-and-queries)

## Diagnostic capture and queries

Open **Debug** from the item or session menu. The snapshot shows current lifecycle blockers, dependency policy and release-lane state using the same rules as execution. A session entry preselects that session's events; capture still covers the entire item. The instance owner can enable detailed capture for 1–60 minutes (30 by default) or stop it early. Restarting the Hub does not extend the expiry. Ordinary reads do not enable capture, advance an item, answer a card or start another turn.

Decision creation/resolution/reopening, dependency handoffs, execution errors and adapter exits retain bounded metadata even with capture off. Capture adds successful tool calls and branch decisions. These records use typed identifiers, reason codes, durations and process status; they do not copy prompts, tool arguments/results, environment variables or raw stderr. A newly enabled window cannot recover detail that was never recorded.

An authorized item agent or active read-only research run can use:

```json
{"action":"explain","item_id":"<item-id>"}
{"action":"events","item_id":"<item-id>","kinds":["adapter.exited"],"limit":50}
```

Pass these arguments to `steve_debug`, using the item ID returned by the item tools. `events` also accepts `severity`, inclusive millisecond timestamps `since`/`until`, `session_id`, `attempt_id` and `event_id`. Follow `next_cursor` with the same filters; it fixes the upper boundary so new events cannot shift older pages. Restart the query to include newer evidence. Stable `diag_…` references identify evidence within this installation and still require authorization. Queries are excluded from tool observation to avoid recording a query loop.

The Web API uses the ordinary authenticated session and selected workspace: `GET /api/items/{id}/debug`, `GET /api/items/{id}/debug/events` and owner-only `POST /api/items/{id}/debug` with `{"enabled":true,"minutes":30}` or `{"enabled":false}`. REST event filters use comma-separated `kinds`; MCP uses an array. Agent access is checked against the current session/run and conversation audience, including replayed tool receipts. A conversation allowed only an item summary cannot retrieve diagnostic events. The tool cannot change capture or repair an item.

The Hub keeps diagnostic metadata in its existing SQLite store, for up to seven days, with logical payload limits of 10 MiB per item and 100 MiB per installation. Detailed capture stops when either limit is reached. Retention and capacity cleanup can remove older diagnostic records; SQLite file allocation, WAL and existing business records are separate from these payload limits. There is no external collector requirement.

Evidence is asynchronous and best effort. Coverage reports known retention gaps, pending work and observed drops; process crashes and the interval between a committed Node event and its diagnostic projection can leave additional unknown gaps. Pending counts are sampled before the database snapshot and may conservatively include work that just finished. If `refresh_required` is true and `loss_count_stable` is false, a recorder commit crossed the read: its loss counts are provisional, so refresh before treating them as exact. Zero drops does not prove completeness. Event `at` is the Hub observation time; adapter details preserve the Node's reported `source_at`, `source_node`, `source_epoch` and process `generation`. Remote clocks may differ. A session-level exit deliberately has no attempt when attribution would be ambiguous. Keep an event's cause reference when investigating the original Node evidence.

Snapshots are sampled reads, not a distributed transaction. `changed_during_read` reports a changed item revision during sampling; unchanged revision does not guarantee every remote process stayed unchanged. Start with `explain`, inspect the relevant events and report facts, hypotheses and missing evidence separately. Diagnosis does not automatically retry, modify code or deploy a release.

## Pairing a separate Node

Remote Nodes are experimental. They run `bin/steve-node`, shipped alongside the Hub and Web in the platform Release bundle. Download the same release for the Node's OS and architecture, verify its manifest, size and SHA256, then extract it into a permanent release directory on that machine. `bin/steve-node --build-info` reports its release version, full commit and target; packaging rejects a Node whose identity differs from the Hub. Source builds remain available for development with `cargo build --locked --release --manifest-path hub-rs/Cargo.toml -p steve-node-cli`.

Before pairing, verify that the Hub has `STEVE_NODE_LISTEN=1` and that its HTTPS reverse proxy forwards `/node/pair` and WebSocket upgrades at `/node/ws` to the Hub. Loopback TCP peers are accepted by default; a proxy on another machine needs its exact TCP peer IP in `STEVE_NODE_PROXY_PEERS` and must overwrite `X-Forwarded-Proto` with its actual incoming scheme. See [Remote Nodes](#remote-nodes) for the HTTPS header requirements and proxy trust boundary. Back up existing configuration, run `steve-hub --check-config` before applying changes, and use the installation's normal restart procedure.

Give the Node its own data directory. Set `STEVE_HOME` explicitly for every Node invocation and in its service environment; never inherit the Hub's data directory. The default, when unset, is `~/.steve-node`. Preserve an existing Node's `node/identity.key`, `node/node.json` and `node/local/` journal when updating it. Do not re-pair an existing identity as a way to reconnect.

The signed-in owner can generate a pairing code in **Devices → Add device** and confirm the pending Node in **Devices**. The authenticated Hub API remains available for the same operations; Agent bearer tokens do not replace the owner session, and API writes require the selected workspace's `x-steve-ws` header.

1. As the signed-in owner, open **Devices → Add device** to generate a pairing code, or call `POST /api/nodes/pairings`. Keep the code private: it expires after ten minutes and can be used once.
2. On the Node, run the following command and enter the code at its stdin prompt. The code is **not** a positional argument. With a publicly trusted HTTPS certificate, no `--pin` is needed. For a pinned certificate, add `--pin sha256:<verified-certificate-DER-hash>`, obtained through a trusted channel.

   ```sh
   STEVE_HOME="<absolute-node-data-directory>" \
     <absolute-path-to-steve-node> pair "https://<hub>" --name "<node-name>"
   ```

3. In Devices, find the pending Node, compare its fingerprint with the CLI output, enter the full fingerprint and confirm it. Save its `node_id` for subsequent checks; do not identify it by name alone. The API equivalent is to read `GET /api/nodes`, match the Node ID and fingerprint, then call `POST /api/nodes/<node_id>/confirm` and check for `{"ok": true}`.
4. Install and start its own user service with the [service commands](#hub-and-node-service-commands), or run the Node in the foreground:

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

`steve-node status` reports the saved pairing and fingerprint; it does not establish that the process is online. Verify Devices shows the same Node ID as enabled and connected, or that `GET /api/nodes` reports `state: "active"` and `linked: true`, and check that the Node's own log contains `linked to the hub`. After restarting only the new service, verify the same identity reconnects. Verify execution separately with a bounded test session when an ACP adapter and a project root are configured.

If deployment fails, stop only the newly installed service. Preserve its logs, identity and journal for diagnosis and retry. If abandoning a confirmed registration, the owner can call `POST /api/nodes/<node_id>/revoke`; a revoked identity cannot reconnect. The local `forget` command only removes pairing files and does not revoke access at the Hub. Restore any backed-up Hub configuration through the same controlled restart procedure if enabling the listener must be rolled back.

## Hub and Node service commands

Every platform bundle includes `ops/steve-service.py` (Python 3.9+). It manages user launchd jobs on macOS and user systemd units on Linux. Use a stable, verified extracted bundle and explicit data directories; Hub and Node must have separate homes. Linux requires a working user systemd session. macOS LaunchAgents run in the signed-in user's session. A new macOS Hub requires the explicit first-time Keychain setup below before `hub install`; the installer will refuse an unsigned home instead of silently selecting another vault backend.

```sh
python3 <bundle>/ops/steve-service.py hub install --home "$HOME/.steve" --release <bundle>
python3 <bundle>/ops/steve-service.py hub start --home "$HOME/.steve"
python3 <bundle>/ops/steve-service.py hub status --home "$HOME/.steve"
python3 <bundle>/ops/steve-service.py hub doctor --home "$HOME/.steve"

python3 <bundle>/ops/steve-service.py node install --home "$HOME/.steve-node" --release <bundle>
python3 <bundle>/ops/steve-service.py node start --home "$HOME/.steve-node"
python3 <bundle>/ops/steve-service.py node doctor --home "$HOME/.steve-node"
```

Both components also support `stop` and `restart`; these commands work without a reachable Hub. `install` is idempotent and does not start the service, create a pairing, change roots or permissions, install a keyhelper, or rewrite `steve.env`. Reinstalling the same configuration leaves a running process alone. `--label` selects a custom service label at initial installation; signed Hub installations retain the label and launch mode in `signing/identity.json`. Changed service definitions have a `.previous` backup. Service metadata is under `<home>/services/`; macOS logs are `<home>/logs/<component>.log`, and Linux logs are in `journalctl --user -u <label>`.

`status` and `doctor` return JSON with the manager state, PID, configured release/build and separate pairing information. A running process and a saved pairing do not establish that a Node is connected or has no active sessions. Verify connection and sessions in Hub Devices. For installations previously managed by another script, pass their existing label and adopt the same release while stopped; the tool does not discover or terminate arbitrary foreground processes.

To update a separate Node, verify and extract the new bundle, inspect the candidate, finish its sessions in Steve and stop its service, then switch its service definition:

```sh
python3 <new-bundle>/ops/steve-service.py node doctor --home "$HOME/.steve-node" --release <new-bundle>
python3 <new-bundle>/ops/steve-service.py node stop --home "$HOME/.steve-node"
python3 <new-bundle>/ops/steve-service.py node update --home "$HOME/.steve-node" --release <new-bundle>
python3 <new-bundle>/ops/steve-service.py node start --home "$HOME/.steve-node"
```

An update refuses a running or unknown service state before changing files. Optional `--wait <seconds>` waits only for the service to stop; it does not drain sessions, stop the service or kill a process. The executable path is pinned to the selected release, and `node/identity.key`, pairing, journals, `roots.json`, `policy.json` and `harness.json` remain in the same home. Keep the previous bundle. Selecting it uses the same stopped-service `update` procedure, but an older Node may reject a newer journal format; verify compatibility before rollback and verify the original fingerprint reconnects. Use **Settings → About** for Hub upgrades: direct service replacement would skip its drain, database backup and migration checks.

## Source-based macOS service installation

Copy `ops/steve.env.example` to `~/.steve/steve.env` and adjust it to your deployment. Keep secrets outside this checkout, using the supported `*_FILE` configuration. On macOS the vault master keys live in the login keychain and only `~/.steve/bin/steve-keyhelper` reads or writes them; `ops/release.sh` rebuilds it only when its own source changes, so rebuilt Hubs keep access to the keys. Run `zsh ops/install.sh` from the checkout to build a source release and install user launchd jobs. Logs are under `~/.steve/logs`.

`ops/release.sh` builds from `main` and switches source releases after checks. `STEVE_REPO`, `STEVE_HOME`, `STEVE_LABEL` and `STEVE_BACKUP_LABEL` support custom locations and service labels. Existing installations must preserve their original signing identities and keychain service: set `STEVE_KEYHELPER_SERVICE`, `STEVE_KEYHELPER_HUB_IDENTIFIER`, `STEVE_KEYHELPER_IDENTIFIER` and `STEVE_LABEL` to the original values in the build environment (they are compile-time identities, not `steve.env` settings) and sign with the original certificate. Changing a namespace or vault backend does not migrate keys. Back up data and verify recovery material before changes.

Existing signed macOS installations can upgrade from **Settings → About** using the local release runner described below. Their original signing identity and installed keyhelper are retained; no vault migration is needed.

## Managed Release bundles and automatic upgrades

Automatic upgrades require a published bundle matching your platform and either Steve's supervisor or the signed macOS local release runner. Release builds target Apple Silicon macOS 14+ and Linux x86_64 with glibc 2.35+ (Ubuntu 22.04+ or Debian 12+); release-asset availability determines which platform can actually upgrade. Windows is not supported. Source development and externally managed containers expose checks only.

Initial installation uses a verified bundle:

1. Select a stable release from [Infranite/steve Releases](https://github.com/Infranite/steve/releases) with a bundle for your OS and architecture. Verify its manifest and SHA256 before extracting it. If no matching release asset has been published, this installation path is not available yet.
2. Install the extracted, paired Hub/Web bundle under `$STEVE_HOME/releases/<version>` and use the service `install --release` command to create `$STEVE_HOME/current`. A bundle contains `bin/steve-hub`, `bin/steve-node`, `bin/steve-keyhelper`, `web/dist`, `release.json` and the runtime `ops/` scripts; do not substitute a source checkout or mix separate frontend/backend builds.
3. Configure `$STEVE_HOME/steve.env`, authentication, the optional public URL, and a supported persistent vault backend. New Linux installations can explicitly use `STEVE_VAULT_KEYSTORE=dir` with `STEVE_VAULT_KEY_DIR` outside `STEVE_HOME`, on protected storage owned by the Hub user with mode 0700, kept mounted across releases. The `file` backend is for isolated development and tests only. Existing macOS Keychain/helper installations use the local release runner below, which signs the downloaded Hub with the original certificate. Do not change their vault backend or namespace.
4. Run `python3 <bundle>/ops/steve-service.py hub install --home "$STEVE_HOME" --release <bundle>`, then `hub start --home "$STEVE_HOME"`. New managed installations run `ops/run-managed.sh` as the foreground service process (`steve-hub --supervise`). An existing signed installation keeps `ops/run.sh` and its original launchd label. Use `hub doctor` and `/healthz` to verify startup.


For a new macOS home, extract the verified bundle under `$STEVE_HOME/releases/<version>` and run this once before step 4:

```sh
STEVE_HOME="$HOME/.steve" python3 "$HOME/.steve/releases/<version>/ops/local-release.py" \
  setup-keychain "$HOME/.steve/releases/<version>"
```

This explicit setup creates a private local signing certificate, signs the already compiled Hub and keyhelper, installs the helper once outside the release directory and writes `signing/identity.json`. It uses the system `security`, `codesign`, `openssl` and `zsh`, with no Cargo or npm. It refuses an existing helper, signing identity, current Hub or nonempty data directory; existing installations must use their preserved identity and the upgrade path. Repeating a completed setup is a no-op. An interrupted partial installation is left for inspection, never overwritten. The initial password file is private; the signed Hub moves it into the login Keychain and removes it on startup. The bundled helper remains untouched during every subsequent upgrade.

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

A signed macOS installation uses the same published Hub/Web bundle without compiling locally. The Hub downloads the matching archive and verifies its size and SHA256 against the release manifest before safe extraction. `local-release.py prepare-bundle` verifies the manifest, Hub build and Web identity, then signs only the candidate Hub with the existing certificate and Hub identifier. It never overwrites `$STEVE_HOME/bin/steve-keyhelper`; the bundled generic helper is not installed during an upgrade. The deployment probe must read the existing vault before switching the service. A missing signing identity, helper or keychain stops the upgrade instead of changing the vault backend or namespace.

The signed automatic runner grants Keychain access only for the account's default `~/.steve` home. A custom macOS Hub home can still use the service commands, but upgrading it requires an explicit terminal invocation of `local-release.py prepare-bundle <verified-extracted-bundle> --version <version> --commit <full-sha>`, followed by `deploy <bundle>`, with `STEVE_HOME` set to that home and its original signing identity; this may show a one-time macOS Keychain prompt. `hub doctor` reports this limitation. Linux managed installations and separate Nodes support custom homes normally.

This path needs Python 3.9+, `codesign`, the original `signing/identity.json` and signing keychain. It does not require a checkout, Cargo, npm or Git. The signed Hub passes the signing password to the one-shot runner through stdin; it is never an argument or a log value. The signing keychain is unlocked only while codesign runs and locked again afterwards. Run upgrades from the authenticated Settings page. Explicit developer source builds remain available as `local-release.py build <commit> --version <version> --from-source --repo <checkout>` and require Cargo, npm, Git and Python 3.12+.

For the first upgrade from an older local runner, its legacy `build` call now downloads and prepares the release bundle by default; it does not compile. That older runner still needs its existing checkout and bootstrap tools to read the new tag's scripts once. Subsequent upgrades use the bundle path directly. A failed download or integrity check never falls back to an unrequested source build.

The operation journal records download bytes, preparation, the actual drain snapshot, backup, probe, restart and health verification. Waiting for active work is a separate phase, not a simulated percentage. Its log is `$STEVE_HOME/logs/release/upgrade-<version>-<id>.log`. `ops/release.sh`, `local-release.py` and the runner share `releases/.release.lock`, so only one release can switch the installation at a time.

Eligible interrupted work resumes through task recovery. Paused work and pending approvals remain waiting; uncertain external delivery outcomes require reconciliation. Keep `$STEVE_HOME/updates` journals, backups and old versions until you accept the upgrade. After a healthy local deployment, pruning keeps the current and previous releases plus the newest three recognized release directories and eight pre-deployment database backups. UUID-named bundle directories are eligible only when their manifest identifies the same version; unrelated directories are not pruned.

## Remote Nodes

With `STEVE_NODE_LISTEN=1`, the owner can use Devices → Add device to generate a ten-minute, one-use pairing code. On the target machine, run `steve-node pair https://your-hub.example --name my-device` and enter the code through standard input. Compare the fingerprint printed by the CLI with the pending device in the owner page, then confirm it before starting the Node service. Pairing does not grant access to any project directories; configure roots separately.

The `/node/pair` and `/node/ws` routes accept only loopback TCP peers by default, for a TLS proxy on the Hub machine. If TLS terminates on a different host, set `STEVE_NODE_PROXY_PEERS` to that proxy's exact IP address as seen by the Hub (comma-separated for multiple proxies). Hostnames, CIDRs, wildcards and unspecified addresses are rejected by `steve-hub --check-config` before deployment. An unset or empty value preserves loopback-only behavior.

Only the actual TCP peer is trusted; `X-Forwarded-For` never grants access. A configured non-loopback proxy must overwrite `X-Forwarded-Proto` with its actual incoming scheme. The Node routes require exactly one value, `https`; missing, duplicate and comma-joined values are refused. Protect the proxy-to-Hub hop with a private encrypted network or tunnel, restrict who can originate from the configured peer, and never copy a caller-provided scheme header. This configuration does not replace pairing, signature verification, owner confirmation or per-Node authorization.
