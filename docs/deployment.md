# Deploying and upgrading Steve

[README](../README.md) gets a local instance running in a few minutes. This page covers running Steve as a long-lived service: a source-based macOS installation, managed Release bundles with automatic upgrades, and how an upgrade behaves.

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

Remote Nodes are experimental. They run `steve-node`, a separate binary from `steve-hub`. Current Release bundles contain the Hub and Web, so build the Node from a compatible source revision on the target OS and architecture:

```sh
cargo build --locked --release --manifest-path hub-rs/Cargo.toml \
  -p steve-node-cli --bin steve-node
```

The binary is `hub-rs/target/release/steve-node` (or under `CARGO_TARGET_DIR` when set). Record its source revision and SHA256 before copying it to the target machine. Keep it in a stable location independent of the Hub's release directory.

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

`steve-node status` reports the saved pairing and fingerprint; it does not establish that the process is online. Verify Devices shows the same Node ID as enabled and connected, or that `GET /api/nodes` reports `state: "active"` and `linked: true`, and check that the Node's own log contains `linked to the hub`. After restarting only the new service, verify the same identity reconnects. Verify execution separately with a bounded test session when an ACP adapter and a project root are configured.

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

A macOS install built locally with `ops/local-release.py` (signed Hub, Keychain vault) also upgrades from this page. It needs `signing/identity.json`, the signing keychain and its password (`signing/keychain-pass` from `ops/sign-setup.sh`; the Hub moves it into the login keychain at start, through `steve-keyhelper`, changes the keychain's password to a new one so that copies of the old file (earlier daily backups) open nothing, and removes the file, so agents can't read it; the upgrade job gets it from the Hub over a pipe, and only for an upgrade of this account's `~/.steve`, through its own `bin/steve-keyhelper`, with a Python from the system's or Homebrew's directories, for the commit the release manifest names when the job reads it again; `ops/release.sh`, `ops/sign-setup.sh` or `local-release.py build` run by hand in a terminal meet a macOS prompt, where you click Allow, never Always Allow; run any other way, including a release the Hub starts with `ops/release.sh`, they stop and ask for the upgrade from this page instead. The signing keychain is unlocked only while codesign runs, and its password never appears on a command line), a checkout at `STEVE_REPO`, and `cargo` (or `STEVE_CARGO`), `npm`, `git` and Python 3.12+ (the first of `python3.13`, `python3.12`, `python3` in `/opt/homebrew/bin`, `/usr/local/bin`, the python.org framework's bin or `/usr/bin` that is new enough; `STEVE_PYTHON` and `PATH` are not used, since that Python gets the signing password). The page lists anything missing. The Hub resolves the release from `STEVE_UPDATE_REPOSITORY` and accepts only a stable version newer than the running one. It then starts a one-shot launchd job (`<label>.upgrade`) with a minimal environment. The job fetches the tag, refuses it when it doesn't point at the commit in the release manifest, and runs that tag's own `local-release.py build` and `deploy --operation <id>`. Each step goes into the same journal, so the page follows it across the restart. Its log is `$STEVE_HOME/logs/release/upgrade-<version>-<id>.log`. Only one release runs at a time: `ops/release.sh`, `local-release.py` and the job all share `releases/.release.lock`.

Eligible interrupted work resumes through task recovery. Paused work and pending approvals remain waiting; uncertain external delivery outcomes require reconciliation. Keep `$STEVE_HOME/updates` journals, backups and old versions until you accept the upgrade. The initial implementation does not automatically prune them.

## Remote Nodes

With `STEVE_NODE_LISTEN=1`, the owner can use Devices → Add device to generate a ten-minute, one-use pairing code. On the target machine, run `steve-node pair https://your-hub.example --name my-device` and enter the code through standard input. Compare the fingerprint printed by the CLI with the pending device in the owner page, then confirm it before starting the Node service. Pairing does not grant access to any project directories; configure roots separately.

The `/node/pair` and `/node/ws` routes accept only loopback TCP peers by default, for a TLS proxy on the Hub machine. If TLS terminates on a different host, set `STEVE_NODE_PROXY_PEERS` to that proxy's exact IP address as seen by the Hub (comma-separated for multiple proxies). Hostnames, CIDRs, wildcards and unspecified addresses are rejected by `steve-hub --check-config` before deployment. An unset or empty value preserves loopback-only behavior.

Only the actual TCP peer is trusted; `X-Forwarded-For` never grants access. A configured non-loopback proxy must overwrite `X-Forwarded-Proto` with its actual incoming scheme. The Node routes require exactly one value, `https`; missing, duplicate and comma-joined values are refused. Protect the proxy-to-Hub hop with a private encrypted network or tunnel, restrict who can originate from the configured peer, and never copy a caller-provided scheme header. This configuration does not replace pairing, signature verification, owner confirmation or per-Node authorization.

## Optional Node disk observations

Set `STEVE_DISK_OBSERVE=on` in the Node process environment to record working-directory disk usage near the beginning and end of session turns. It is off by default; `off` stops new records. This does not change execution permissions, disk admission or cleanup. Samples are asynchronous observations with possible concurrent writers, not reliable baselines, attributable growth or permission to delete anything. They do not yet produce per-principal or per-project historical estimates.

Sampling budgets are configurable with `STEVE_DISK_OBSERVE_ENTRIES` (default `100000`), `STEVE_DISK_OBSERVE_DEPTH` (`64`) and `STEVE_DISK_OBSERVE_MS` (`500` milliseconds). Exhausted budgets preserve incomplete results. Each Node starts at most one sampler at a time. Disabling observations or shutting down cancels traversal at its next check; path validation and a single blocked system call cannot be interrupted. The async worker can stop waiting, but process exit may still wait for that blocking call to return.

Records stay in the Node journal. `STEVE_DISK_OBSERVE_ROWS` defaults to `1024`; `STEVE_DISK_OBSERVE_KEEP_MS` to `604800000` (seven days). Enqueuing new records removes expired or oldest completed statistics as needed. In-flight records are not evicted; a full queue increments a skipped counter. Values must be positive, representable integers. Invalid settings warn and disable observations without refusing Node startup. The Node host API offers paged readback; no Web or MCP endpoint is added.
