# Deploying and upgrading Steve

[README](../README.md) describes the Homebrew installation and source-based development setup. This page covers first-time configuration, user services, managed releases and upgrades.

- [Homebrew installation](#homebrew-installation)
- [The first meeting](#the-first-meeting)
- [Reconfiguring an installed Hub](#reconfiguring-an-installed-hub)
- [Hub and Node service commands](#hub-and-node-service-commands)
- [Source-based macOS service installation](#source-based-macos-service-installation)
- [Managed Release bundles and automatic upgrades](#managed-release-bundles-and-automatic-upgrades)
- [Pairing a separate Node](#pairing-a-separate-node)
- [Remote Nodes](#remote-nodes)
- [Harness setup](#harness-setup)
- [Diagnostic capture and queries](#diagnostic-capture-and-queries)

## Homebrew installation

```sh
brew install infranite/tap/steve
steve setup
```

The official [tap](https://github.com/Infranite/homebrew-tap) downloads a versioned installer archive, verifies its SHA256 and installs the complete precompiled Hub, Node, Web and CLI. The CLI, setup, service management and signed-Hub artifact upgrade paths are native Rust; no Python interpreter, Steve compiler or source checkout is required. Supported Steve binaries are Apple Silicon macOS 14+ and Linux x86_64 with glibc 2.35+; Homebrew may require a newer OS under its own support policy. Intel macOS and Linux ARM binaries are not available.

On a terminal `steve setup` is a wizard: it asks its questions first (the role, who can reach the Hub, which ACP adapters to install), then runs the steps and shows their progress. Off a terminal (a pipe, `TERM=dumb` or `STEVE_PLAIN=1`) it prints plain lines and reads typed answers. Its texts are Chinese or English: `--lang zh-CN|en` first, then `STEVE_LANG`, `LC_ALL`, `LC_MESSAGES`, `LANG`, on macOS the system's language list, and Chinese when none says.

`steve setup` asks you to select Hub or Node; `steve setup hub` and `steve setup node --hub https://<hub>` select it explicitly. Default data directories are `~/.steve` and `~/.steve-node`, respectively; `--home <absolute-directory>` selects a separate directory. Setup copies the immutable package into a private `<home>/releases/<version>` directory before signing or registering anything. It never takes over an unknown existing home or identity. When the home already holds an installation this setup cannot continue (another version's, or the other role's), the wizard asks whether to install afresh, and `--fresh` does so without asking: the old service is stopped and unregistered and the whole directory is renamed to `<home>.bak-<time>`. A Hub whose vault keys are in a directory of their own (`--vault-dir`, and always on Linux) has that directory renamed the same way, to `<directory>.bak-<time>`: those keys open the data set aside, and the new installation starts with new ones. Nothing is deleted, so moving both back and registering the service again restores the old installation. Without `--fresh`, plain setup stops and names the option. Its private setup receipt allows the same interrupted setup to continue while retaining the completed configuration and keys. A package upgrade can resume an earlier setup that has not yet signed, paired or registered anything, after verifying its receipt and copied files and backing up the receipt; an existing identity is never replaced.

Interactive input validation explains the error and asks for that field again, keeping the earlier answers. EOF, terminal cancellation and filesystem or ownership errors still stop setup and preserve its progress. Explicit invalid command-line options fail instead of opening another prompt. Ordinary setup resumes its own unfinished installation and refuses to overwrite receipt-owned files that have been edited. For an installed managed Hub, use the separate [`steve setup --reconfigure` workflow](#reconfiguring-an-installed-hub) to review and save supported configuration changes. Preserve the original home, setup receipt and identity; do not remove them to bypass an ownership check.

For a Hub, the wizard asks no sign-in questions. When the configuration checks pass, it issues the owner's first access token. The owner is your account name. The token is shown at the end and kept in `secrets/owner.token` (mode 0600). Sign in at the browser URL with that user name and token, and see [Access tokens and clients](#access-tokens-and-clients) for more tokens. Single sign-on through OIDC stays optional: an existing `STEVE_OIDC_*` configuration keeps working. The Hub verifies the issuer's HTTPS certificate against the bundled public roots and the system trust store, so an issuer behind a private CA works once that CA is trusted by the Hub's OS. The wizard asks who can reach the Hub: other devices on your networks (`0.0.0.0:8440`, the default, which writes the existing `STEVE_ALLOW_ANY_HOST=1` setting), this machine only (`127.0.0.1:8440`), or an address you type. `--listen <IP:port>` answers it on the command line, and plain setup asks for the address itself. The browser URL is not asked for: it follows from the listening address (the web client on the next port of local loopback, since `0.0.0.0` is not a browser destination). `--public-url <URL>` names another one, `steve setup --reconfigure` changes it later, and setup asks only when the Hub listens on a specific address that is neither. A public HTTPS URL still requires your reverse proxy and network configuration. Existing explicit configurations are retained. macOS setup prepares and signs the private Hub/helper copies, with system authorization prompts when required. Linux asks for a separate protected `0700` directory for its persistent vault keys. Setup does not enable development login or copy another harness's credentials.

For a Node, supply a reachable HTTPS Hub and enter the code generated in **Devices → Add device**. Compare and confirm the displayed fingerprint in Devices before starting it. `--pin sha256:<verified-certificate-DER-hash>` is available for a certificate you have explicitly verified. Roots start empty, and registration does not move projects, grant directories or enable multi-Bot execution. See [pairing verification](#verify-and-recover-a-node).

Setup registers the Steve user service, which is configured to start in subsequent user sessions. For a Hub, the wizard then shows the address, user name and token to sign in with and offers to start the Hub now; it waits for the Hub to answer and offers to open the browser. Plain setup and a Node's setup only register the service. Start, check and diagnose the selected component with:

```sh
steve hub start
steve hub status
steve hub doctor

steve node start
steve node status
steve node doctor
```

Use only the commands for the role installed on that machine. Both roles also support `stop` and `restart`; `--home` and the original `--label` select a custom installation. Node connection and idle state must still be verified in Devices. Principal-specific model authentication and the [harness setup gap](#harness-setup) are separate from installing Steve.

Homebrew manages the installed package and CLI. `brew upgrade infranite/tap/steve` downloads a newer package without switching a running Hub or its database. The CLI reports its **package version** with `steve --version`; `steve hub status` reports the configured runtime separately, and `/healthz` verifies the actually running Hub; on a terminal `steve hub status` and `steve hub doctor` show that running version too (their `--json` output is unchanged). Keep using **Settings → About** or `steve upgrade` for supported signed-Hub upgrades: the existing coordinator retains task drain, backup, schema checks, activation and rollback. A Node update uses the [stopped-service procedure](#hub-and-node-service-commands). Services run from their private runtime copies, so Homebrew cleanup does not remove their executables or data.

Homebrew does not manage these services through `brew services`. Stop and unregister the relevant Steve service before removing its Homebrew package:

```sh
steve hub stop
steve hub uninstall
brew uninstall steve
```

Use `node` instead of `hub` on a separate Node and verify its sessions in Devices before stopping it. `uninstall` requires a known stopped service, also unloaded from launchd on macOS, and preserves a backup of its registration. It does not remove the private runtime, configuration, keys, data or Node pairing; keep those for diagnosis or a separately planned removal. Revoke a retired Node from Hub Devices when you intend to remove its access.

Without Homebrew, use the verified installer archive from the same [Release](https://github.com/Infranite/steve/releases), then run `<installer>/bin/steve setup`. The separate runtime archives and `steve-release.json` remain compatible with existing upgrade clients; Homebrew uses `steve-installer.json` and the installer archives.

### A second installation under the same user

`--label <label>` gives an installation its own service label (launchd job or systemd unit) and, for a Hub, its own signing identity, so it can sit next to the usual one. Use it together with its own `--home` and `--listen`. On macOS, `--vault-dir <directory>` keeps that Hub's vault keys in a directory of their own (`0700`, outside the data directory) instead of the login keychain, where the usual installation's keys live. For example:

```sh
steve setup hub --home ~/steve-trial --listen 127.0.0.1:18440 \
  --label io.github.infranite.steve-trial.hub --vault-dir ~/steve-trial-keys
```

Manage it with the same `--home` and `--label`, as in `steve hub status --home ~/steve-trial --label io.github.infranite.steve-trial.hub`.

#### Its harnesses' own state

A second installation is separate from the usual one in everything Steve keeps, but not in what the harnesses keep. The owner's main conversation, the first meeting and the model listing start each harness with the service's environment, so Claude Code and Codex use the OS user's own state directory (`~/.claude`, `~/.codex`): the same history, memory and settings you use yourself and the usual installation uses. An installation made to try something out, to test a build or to demonstrate Steve should not write there. Before its first start, give each of its harnesses a directory of its own in its `steve.env`, with the variable the harness reads its state directory from (`CLAUDE_CONFIG_DIR` for Claude Code, `CODEX_HOME` for Codex; for another adapter, the `state.env` it declares in the harness manifest):

```sh
mkdir -p ~/steve-trial-harness/claude ~/steve-trial-harness/codex
cat >> ~/steve-trial/steve.env <<EOF
CLAUDE_CONFIG_DIR='$HOME/steve-trial-harness/claude'
CODEX_HOME='$HOME/steve-trial-harness/codex'
EOF
steve hub start --home ~/steve-trial --label io.github.infranite.steve-trial.hub
```

`steve.env` holds literal values, so the paths are written out in full (the shell does that above). The directories start empty: the harness has no settings and no login there. Put the settings it needs into them (`settings.json` for Claude Code, `config.toml` for Codex) and sign in with the harness's own command and the same variable, as in `CODEX_HOME="$HOME/steve-trial-harness/codex" codex login`. Do not copy another installation's `auth.json` or refresh tokens. Sessions that run for a principal get their own directory under the installation's home either way ([`STEVE_PRINCIPAL_STATE`](#harness-setup)); the owner's starts from the settings found in the directory configured here.

Two things stay shared. The owner's main conversation works in the OS user's home directory with that user's access, so a harness still reads the instruction and configuration files it looks for in its working directory, and Steve itself can read and change whatever that user can: use a trial for what you would do at your own shell. And a real harness spends the same account's quota whichever directory it keeps its state in.

## The first meeting

The first time the instance's owner opens an empty Hub in the browser, a short tour ends by offering a first meeting. Accepting it makes Steve open a conversation in its own main conversation; declining it, or skipping the tour, is remembered by the Hub for every browser. Other users only see the tour.

In the meeting Steve asks what to call you and what you use it for, and then, one at a time and only after you agree in the conversation:

- **Your profile.** It may read a small, recent part of what is already on the Hub's machine (the session history and global instruction files coding agents left in the home directory, the global git configuration, the projects under the home directory) and saves what it learned as your name, a short description and remembered facts and preferences under 我. It is told not to read or save secrets.
- **The models.** It lists the models each installed harness offers and picks, per purpose, a fast one for routing and titles and the strongest for research, drafting and orchestration. Only models a harness reports are accepted. Settings · Models shows and changes the result.
- **Bots.** From what you do and the Bots there are already, it proposes two to four Bots and creates the ones you agree to. The Bots page shows and changes them.

Every one of these writes goes through the same validation, audit log and setting revisions as the corresponding page, so each can be reviewed and undone there. Settings · Activity log lists them for the instance's owner, newest first: when the meeting began and how it ended, what each step saved (the profile's fields and the number of remembered facts, the purposes set, the Bots created), and under them the single writes, each with who did it and, opened, what changed. The meeting runs as the owner's main conversation does, with the access of the OS user the Hub runs as, and it uses the model of Steve's own Bot, so it spends that harness's quota.

The meeting ends when Steve finishes it, when you say you want to skip it or press **Skip introductions** under its welcome, or by itself after twelve of your messages. Until then, what you say in the main conversation goes straight to Steve without the routing step, except a message that names a Bot or an item. After it, changes go through the usual pages and change cards; its tools no longer exist. Settings · General · Onboarding starts the tour, and with it a new meeting, again.

A Hub driven without the web client can use `POST /api/onboard/start` and `POST /api/onboard/skip` (the owner's access token) for the same two actions.

### The activity log

Settings · Activity log (the instance's owner only) shows the workspace's audit trail: who changed what and when, newest first, fifty at a time. An entry opens to what its writer recorded: for a changed setting the old and the new value, for a created Bot its name. Ids are shown by what they are called now. The same list is `GET /api/audit?limit=<1..200>&before=<entry id>` with the owner's access token: `{entries: [{id, ts, actor, action, target, detail}], names: {<id>: <name>}, more}`; pass the last entry's `id` as `before` for the next page. The Hub does not change or remove entries.

### Models without the meeting

Steve names no model by itself: every judgment runs on the model of Steve's own Bot, which is the harness's default and usually its strongest. For the frequent, small judgments (routing a message, titles, the disclosure check, a flow's Bot nodes) that is slow, so they get one default whether or not the meeting happened: once the harness Steve runs on has reported its models, the Hub puts those four purposes on that harness's quick model, one time per workspace and only where nothing is set. Which model counts as quick is a property of the harness adapter (`fast_models`: words that name them, best first; the built-in adapters declare theirs, and an adapter in the harness manifest may declare its own). Of the models the harness offers, the one picked has such a word as a whole word of its name; when several do, the one on the same route as the harness's default model (`route/name`), then the newest by the numbers in its name. A harness that offers no such model leaves everything on Steve's model.

The result is an ordinary override under Settings · Models: change it or put the purpose back on Steve there, and it stays that way. `STEVE_MODELS_FAST` (and `STEVE_MODELS_CHECK` for the disclosure check), when set, name these models instead.

## Reconfiguring an installed Hub

Run the interactive maintenance wizard as the OS user who owns the Hub:

```sh
steve setup --reconfigure
```

For a custom installation, add `--home /absolute/path/to/hub`. This requires a completed managed Hub setup, its original `setup.json`, registered service, and a current release under that home's `releases` directory. The installed Hub must support `--check-config`; macOS also requires its original signing identity and keyhelper. Upgrades may leave the original version in installation receipts; maintenance validates the current release separately. Unknown, incomplete or retired installations are refused. `steve setup node --reconfigure` is refused.

Defaults come from the current `steve.env`, including intentional edits since setup. The parser reads literal `KEY=value` or `export KEY=value` assignments without sourcing shell code. Other assignments, comments, spacing and line endings are retained; duplicate keys, expansions, commands and unsupported syntax are rejected with a line number. The wizard edits listening addresses and their shared port, wildcard permission, the browser URL, and the existing OIDC issuer and client ID. When a browser URL list is configured, changing the primary URL preserves its other aliases. The OIDC secret must already have a file reference: its path and contents are retained, and the wizard does not request or rotate it.

The wizard previews the changes and asks for confirmation with **No** as the default. Before saving, it checks the candidate environment with the currently installed Hub's `--check-config` and rechecks that the installation has not changed while you were answering. It then stores private backups of `steve.env` and `setup.json`, saves both, and reads them back along with the protected installation files. A failed save restores the originals when they still match the transaction; concurrent external edits are not overwritten. If both configuration and receipt already agree, a repeated run leaves them untouched. Signing identity, vault configuration, databases, service registration, current release and Node pairing are preserved.

Saving does not start or restart a service. Use the original home and label when ready to apply the saved configuration:

```sh
steve hub restart --home /absolute/path/to/hub --label <original-service-label>
```

Omitting `--label` reads the label from the existing service registration; omitting `--home` selects `~/.steve`. The command's local checks do not test reverse-proxy routing, OIDC discovery or browser login. After restarting, verify `/healthz`, browser access and OIDC login; update the registered callback URL at your OIDC provider if the browser address changed.

Backups and transaction records remain under `<home>/setup-reconfigure/`. If interruption leaves a `pending` transaction, rerun `steve setup --reconfigure` with the same home and confirm recovery. Recovery verifies the installation and preserves a committed change or restores an uncommitted change from its private backups. If files have been edited externally, it retains the recovery record and refuses to overwrite them. After recovery, run the wizard again for further changes. EOF or declining confirmation before saving leaves the configuration untouched.

## Backups

A running Hub makes a copy of its data once a day, at or after 03:30 local time, in `<home>/backups`; a Hub that was not running then makes the day's copy a few minutes after it next starts. Copies are kept 14 days, and the directory and the files are readable by the owner only:

- `daily-YYYYMMDD.sqlite3` is an online backup of the database, kept only after SQLite's `quick_check` passes.
- `daily-YYYYMMDD-files.tar` holds what the database alone cannot bring back, as far as it is in the home: attachments (`data/files`), item deliverables (`workspaces`), the signing identity (`signing`, without a keychain password file), the secret files (`secrets`) and `steve.env`.

**Settings → Data and privacy** shows the instance's owner where the database and the copies are and when the newest daily copy was made; `GET /api/system/backup` returns the same. The copies taken before each upgrade (in `<home>/updates`, or `pre-*.sqlite3` in `<home>/backups` for a local release) are separate and are made either way.

The copies are on the same disk as the data: they protect against mistakes and a corrupt database, not against losing the machine or the disk. Copy `<home>/backups` elsewhere with your own tooling for that. A vault key directory outside the home (`--vault-dir`, `STEVE_VAULT_KEY_DIR`) and keys in the login keychain or Secret Service are not in the tar; without them the vault's secrets in a restored database cannot be read, so keep them with your off-machine copy.

Set `STEVE_BACKUP=off` in `steve.env` to turn the daily copy off, for example when the home is already covered by snapshots. A source-based installation's `ops/backup.sh` job writes the same file names, so where both run the day's copy is made once.

To go back to a copy, stop the Hub (`steve hub stop` with the installation's `--home` and `--label`), move `data/steve.sqlite3` and its `-wal` and `-shm` files aside, put the chosen `daily-YYYYMMDD.sqlite3` in its place as `data/steve.sqlite3`, and start the Hub. A copy older than the running release is migrated forward at start; a copy made by a newer release than the one running is refused.

## Access tokens and clients

Besides a browser session, a client signs in with a user's name and one of that user's access tokens. The Hub issues the token and keeps only its hash. The client gets a credential of its own and sends it as `Authorization: Bearer <credential>` from then on.

- `GET /auth/hello` says which Hub this is, its API version and which sign-ins it offers.
- `POST /auth/token {"user": "...", "token": "stt_...", "client": {"kind": "web|mac|ios|cli|...", "name": "...", "platform": "...", "version": "..."}}` returns `{client_id, credential}`.
- `POST /api/access/pairings` (authenticated) issues a 5-minute single-use `stp_...` pairing code and answers `{id, code, name, expires, links, …}`. `links` has one entry per address a device may reach the Hub by, the public one before those of the local network: `{url, kind: "public"|"lan", link}`. `link` is what the QR code holds: `<that address's web page>/#/pair?v=1&code=stp_...&name=...&pub=...&lan=...`, a page any browser opens. The apps read the same link, also as `steve://pair?` with the same query.
- `GET /api/access/pairings/{id}` reports when a device claims it, and `POST /auth/pair {"code": "stp_...", "client": {...}}` exchanges that single-use code for a dedicated client credential without creating a reusable access token.

The Hub records each client's kind and name, and the route it last came by. Routes are `loopback`, `lan`, `tailnet`, `proxy` or `public`, with `+tls` added when the request arrived over TLS. Forwarding headers are believed only from this machine or the local network. Failed sign-ins from another address are slowed down after ten tries in ten minutes.

Manage tokens and clients on the Hub's machine, also when it does not answer. `steve-hub` is the installation's own binary, `<home>/current/bin/steve-hub` (`~/.steve/current/bin/steve-hub` for the usual installation); it is not on the `PATH`. For an installation in another directory, name it with `STEVE_HOME=<home>` in front of the command, as the line `steve setup` prints when it is done does.

```sh
steve-hub --access token create [--user NAME] [--name LABEL] [--days N]   # shown once
steve-hub --access token list
steve-hub --access token revoke ID        # signs out the clients that used it
steve-hub --access client list
steve-hub --access client revoke ID
```

On a Hub nobody has signed in to yet, `token create` makes `--user` (default: your account name) its owner.

Single sign-on through OIDC is optional. The owner sets it up in **Settings · Access & clients** with an issuer, a client ID and a secret. It takes effect without a restart. The secret is kept in `secrets/oidc-client-secret` (mode 0600). The page shows the callback to register with the provider. An `STEVE_OIDC_*` environment configuration takes precedence and is shown read-only. When the web client has a port of its own, its `/auth/…` paths redirect to the Hub's port, so a callback registered on the web address works.

## A web client apart from the Hub

The web page can run on its own origin instead of the Hub's port.

- **New installations** set `STEVE_WEB_PORT` to the Hub's port plus one. The Hub then serves the pages on that port, on the same hosts, and its own port answers the API only. A browser that asks the API port for a page is sent to the page port.
- **Without `STEVE_WEB_PORT`**, pages and API share the Hub's port as before.
- **On another machine or a static host:** `steve-hub --serve-web --listen <address:port>` serves only `STEVE_WEB_DIR`. Any static host also works, because the built pages are plain files.

Opened somewhere no Hub answers, it asks for the Hub's address once, checks it with `/auth/hello`, keeps it in the browser, and then shows the sign-in.

- **Cross-origin access.** The Hub answers cross-origin calls with credentials only from the origins of web clients that signed in there, and from those listed in `STEVE_CLIENT_ORIGINS` (comma-separated). The sign-in paths and a `401` are readable from any origin; a `401` to an origin that is not let in also carries `x-steve-origin: refused`, and the web client then offers signing in by token, which lets that origin in (a browser sign-in returns only to the Hub's own addresses). A page whose calls are refused outright (a preflight answered `403`) is shown the same sign-in when the Hub still answers `/auth/hello`.
- **Same machine.** A Hub on the same host as the page (another port) is the same site, so its sign-in cookie works over plain HTTP.
- **Another machine.** A Hub on another host needs HTTPS: the Hub then sets its cookie with `SameSite=None; Secure`.
- **Cookie names.** Browsers keep cookies per host, whatever the port. The Hub therefore names its cookies with its port (`steve_client_8440`, `steve_session_8440`, …), so several Hubs on one host stay signed in side by side.

## Several Hubs, and pairing a device

The web client keeps a list of Hubs and switches between them. It is the same screens in a browser and inside the macOS and iOS apps.

- **Where.** The Hub switcher is at the top of the sidebar (on a phone: the **Me** page). It lists the saved Hubs and has **Add Hub…** and **Pair a new device…**. The sign-in screen and the "cannot connect" screen show the same list. **Settings · Access & clients** has the full list, where a Hub can be forgotten, and <kbd>⌘</kbd> <kbd>K</kbd> has the same entries. A link can open them directly: `#/hub/add`, `#/hub/pair`. In a browser, a pairing opened by a link makes its code only after a click.
- **The list** is kept in the browser (`localStorage`), per page origin. Inside the apps it is the app's list, with the credentials in the Keychain.
- **Adding** takes a Hub address or a pairing link. The address is checked with `/auth/hello` before it is saved. The address of a Hub's pages (what the address bar shows when the web client has a port of its own) works too: the page port names its Hub in `/hub.json`.
- **Switching in a browser** goes one of two ways. When the page can hold a session at the other Hub (same host on another port, or an HTTPS Hub), it stays where it is and calls that Hub. Otherwise (plain HTTP on another host, or an HTTPS page and an HTTP Hub) the browser goes to that Hub's own page. The saved list travels along in the address fragment, which is not sent to any server, holds addresses and names only, and is added to the other page's list only after the person agrees. Some browsers refuse the cookie of a Hub on another site (Safari, private windows); the sign-in then says so and offers that Hub's own page.
- **Pairing.** **Pair a new device…** shows a QR code that is valid for five minutes and for one device, with the address it uses; when the Hub has several addresses, pick the one the device can reach. The code holds a link (see `POST /api/access/pairings` above). A phone's camera opens it in the browser: the page names the Hub, asks, and signs this browser in. A page claims a code only for the Hub on its own host or the Hub it already uses; a link for any other Hub is opened at that Hub's own page, after a warning. The iOS app's scanner reads the same code, and the page offers to open the app when it is installed. The link can also be copied and pasted into another device's **Add Hub** field. The pairing code is removed from the address bar as soon as the page has read it.
- **Which addresses a pairing offers.** `STEVE_PUBLIC_URL` when it names a public host; then the addresses of the local network: a `STEVE_PUBLIC_URL` on a private address or a name that only resolves inside a network (no dot, or under `.local`, `.lan`, `.home`, `.home.arpa`, `.internal`), the address the browser that asked came by, and the addresses the Hub listens on (every active interface when it listens on `0.0.0.0`). Loopback addresses and names under `localhost` are never offered. A Hub that listens on loopback only has nothing to offer another device, and the dialog says so.

## macOS and iOS clients

Native macOS (`SteveMac`) and iOS (`SteveIOS`) clients live in `apple/SteveClient.xcodeproj` atop the shared Swift package `apple/Packages/SteveClientKit`. Each is a shell around the web client: it shows the bundled pages through a gateway on loopback that adds the client's credential from the Keychain, so the credential never reaches script in the page.

- **Default local Hub.** On launch, the client connects to the local Hub (`127.0.0.1:8440`, or `STEVE_HUB_URL` when set in the process environment). With no Hub, or one that does not answer, it shows the web client's own screen for adding or choosing one.
- **Hubs and pairing** are the web client's screens described above. The app supplies what a browser cannot: the Keychain, the camera on iOS, the `steve://pair` link, and a confirmation of its own before any pairing is claimed (script in the page can ask for a pairing, not confirm one).
- **Route selection.** When a pairing names a public address, the client connects over it by default and falls back to the LAN addresses if it is unreachable; without a public address it connects over LAN.
- **macOS menu bar status item.** `SteveMac` provides a live status bar icon (`MenuBarExtra`) displaying connection phase, route (`loopback` / `LAN` / `public`), Hub version, active workspace, running sessions, pending Inbox decisions and online devices, with quick Hub switching and entries that open the main window at **Add Hub…** and **Pair a new device…**.
- **Building from source** (macOS 14+ with Xcode):

```sh
./scripts/build-apple-client.sh
```

Individual targets can also be run directly (`./scripts/build-apple-client.sh test`, `mac`, `ios`, or `ios-sim`).

## An external channel service

A messaging system Steve has no built-in adapter for can be connected through a separate channel service: a program that signs in to that system as you and speaks Steve's external channel protocol over a WebSocket. The service runs on its own (on your machine or LAN); the Hub dials out to it, so the Hub opens no port for it, and a crash or restart of the service only pauses the channel.

- **Connect:** in **接入 · 外部渠道**, enter the service's address (`wss://…`; plain `ws://` only to `127.0.0.1` or `::1`), the access token the service issued for this Hub, and, for a self-signed certificate, its pin (`sha256:<64 hex digits>`). The Hub checks the address, says hello, asks the service who is signed in, and the connection becomes yours.
- **Receiving** follows the connection's `msg.receive` capability. Messages written before the connection was turned on, read back after an interruption, or your own, are only recorded.
- **Sending as you** (replies, reactions, recalls) goes through the service, which may refuse or limit it; a service that forbids writes leaves the connection read-only.
- **The protocol** a service implements, with its conformance checklist and test vectors, is the documentation of `steve_channels::adapters::ext_ws::protocol` (`hub-rs/crates/steve-channels/src/adapters/ext_ws/protocol.rs`). The Hub's test suite runs a fake service against it (`hub-rs/crates/steve-channels/tests/ext_ws.rs`).

## Workflow execution

A multi-Bot work graph coordinates ordinary item sessions. A child inherits the parent's `requested_by`, workspace, project and routing tags; its Bot remains the one assigned by the graph. Execution is admitted only when the parent and current child have the same confirmed request principal. A current workspace member is a user principal; an item with no requester explicitly uses the workspace service principal. An unknown or departed requester is blocked for workflow execution rather than silently changed to the service principal. Existing live Node bindings must agree with that identity. Cross-principal collaboration is refused.

Children use the ordinary Node binding and execution-policy path, including Bot membership and enabled state, roots, permission modes, Node ceiling, approvals, principal harness state and environment settings. Workflow itself does not require the `os-sandbox` or `principal-isolation` capabilities and does not claim to provide multi-principal OS isolation. Confinement remains the Node's independently configured policy. The current remote Node single-owner workspace restriction still applies; remote session placement still accepts only project-free work in scratch workspaces.

Git projects must be active in the same workspace with item worktrees enabled. Each child uses a separate worktree; work without a project uses a separate item directory. A stopped, disconnected or unauthorized Node, an unmet routing tag, a changed principal or an unavailable Bot prevents new execution. Work never falls back to a Hub harness. The item page reports admission failures under collaboration blockers; fix the stated configuration or identity problem, then resume a paused graph or retry a failed unit within its attempt budget.

The saved graph, attempts, child items, sessions and Node turn ledger survive restarts. Recovery reuses an inserted session even if its first Node binding was interrupted, and reconciles an already running turn before continuing. Repeated scheduling does not create a second child or replay a claimed turn. A retry creates a new attempt and retires the old one; an old session cannot gain access by detaching from the graph. A failed work unit closes its child item as failed after the turn ends, and a cancelled one closes it as cancelled; recovery also closes older current or retired attempts once their turns are no longer running. A retired failed delivery keeps its failed outcome, while a retired attempt without failed delivery is cancelled. The result remains in the graph history for the coordinator to replan or retry. A unit out of attempts holds every unit after it, and the coordinator is told instead of the graph waiting as queued. A pause marks the exact turn it stopped: only that turn's end counts as the pause, so a later turn you stop yourself still fails its unit. A stop alert left open before a restart expires when the current graph shows work running or a definite wait; actual user questions and uncertain external effects remain open. Pauses and prerequisite waits hold queued follow-ups too. Dependent work waits for successful delivery, the upstream turn to end and its item to finish closing. Independent review and integration acceptance remain required before the parent can finish; workflow delivery does not authorize a release or deployment.

The items list reads the work graph's pause flag, cycle and failed units. A parent with a paused graph in its current cycle is grouped under **Paused**, even when the parent item's lifecycle status is still `run`; its row names failed current-unit child attempts. That applies to a graph you paused. A graph its coordinator Bot paused without asking you anything waits for you instead: Steve opens a card with the coordinator's pause reason (when its turn ends, or within a few minutes for a graph paused earlier), the item moves to **Waiting for you**, and Steve does not continue the coordinator over its own pause. Resuming the graph withdraws the card. A pause recorded before this release counts as the coordinator's unless it came from pausing the item itself. An active parent lists only unfinished workflow children as current waits. A work graph from an earlier cycle is labeled as a prior-cycle record, not as the current cycle's pause. Historical closed children remain available in the graph and item history without being shown as current waits.

The item page derives one state for the item — waiting for you, a work graph to replan, unavailable or paused collaboration, working, queued, idle or closed — and every place on the page (header, "now" card, the tree's root) shows that state. An item that coordinates a work graph or started other items lists its team in the side column, a compact agent tree of who started whom, and opens the full tree from there. The coordinator's units follow dependency order; each unit's earlier attempts stay beside it, dimmed, and items forked from any node hang under it. Dependencies are not drawn as edges: a waiting unit names what it waits for. The direct children sit side by side while they fit and otherwise, like every deeper level, hang from a rail. The header counts agents, running work, current failures (with retry) and decisions waiting for you, and shows the estimated cost of the whole subtree from `GET /api/stats/tree/{id}`, which returns the item and the items started from it (at most eight levels and 200 items) with each one's own usage and agent time. Selecting a card opens its detail beside the page, or as a sheet on narrow screens; open the linked child item or session for its full record. A running unit can be stopped individually. Reworking a completed unit previews its downstream invalidation, requires active work to stop and settle, and uses the existing attempt budget and revision checks before resetting; earlier child items remain available in the archive. A running workflow unit is labeled confirmed execution only when the Hub has applied a recent assistant, tool, permission or plan event for the exact active session, turn, attempt and Node, after that attempt started. The Hub receive time must be less than ten minutes old, and the Node must still be active and recently seen. Missing, stale or uncertain evidence is shown as active but unconfirmed; it does not change scheduling or approval authority. The relation graph, opened from the item menu, can show blocking dependencies or provenance, with missing references visible and ownership edges distinct from dependencies. On narrow screens the same relations appear as a structured list; selecting a node shows its detail before an explicit open action. Archived attempts are read-only links to their child items. They do not count as current-cycle delivery.

The work graph API shows bounded unit summaries and the latest 20 plan changes; full audit history remains in storage, and full non-Git deliverables are available from the child item's outputs. A provider usage-policy refusal or invalid API request has no review verdict: Steve records the failed attempt, keeps successful upstream units, and requires a user to retry after resolving the provider policy or request configuration. A graph from a prior item cycle does not grant acceptance for the new cycle. After the old graph is paused and every old execution has stopped, replanning archives its units with their original verdicts and starts pending units for the current cycle. Old successes remain visible as history but never satisfy the new cycle's review or integration gates.

For a design or report delivery, Steve saves the delivery text as a Markdown file in `<home>/workspaces/<item>/outputs` and registers its path as an item output. Declared files produced in a Node workspace are copied to the same directory before delivery completes, so a removed worktree or an offline Node does not hide them. The database stores the output path and state, not the file body. Restore both the database and `workspaces` from the same daily backup. A design-only goal, or one that explicitly defers implementation until design confirmation, opens a user-only decision bound to the delivered design and goal version; choosing implementation continues the same item, while choosing design only closes that design cycle.

At startup, the Hub repairs old output records that point to a prose summary when the full saved text can still be found. Before it changes a record, it creates a verified `pre-delivery-recovery-*.sqlite3` copy in `<home>/backups`. The repair is safe to retry after a failed start. Files that were deleted before the upgrade cannot be restored this way.

## Diagnostic capture and queries

New items, including ordinary conversation items and workflow child items, automatically start one 30-minute window of detailed capture. The deadline is persisted when the item is created; queries, restarts and continuing the item never extend it. Older items are not enabled retroactively. Once a window expires or the owner stops it, it stays stopped until the instance owner explicitly starts another 1–60 minute window in **Debug**. There is no new environment switch.

Open **Debug** from the item or session menu. The snapshot shows current lifecycle blockers, dependency policy and release-lane state using the same rules as execution. A session entry preselects that session's events; capture still covers the entire item. The instance owner can stop capture early or enable a new window (30 minutes by default). Ordinary reads do not enable capture, advance an item, answer a card or start another turn.

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

## Several machines

Steve runs work on Nodes. The Hub is always one of them: its own Node, `local`, runs in a process of its own by default, so a Hub restart or upgrade leaves its turns running; `STEVE_NODE_MODE=local` keeps it in the Hub's process. Any other machine can become a Node as well. A typical arrangement is an always-on Hub with other machines added as Nodes, but none is assumed: one machine on its own is a complete Steve.

- **Reaching the Hub:** a Node dials the Hub, never the other way round. It can use the Hub's HTTPS address (a reverse proxy that terminates TLS, see [Remote Nodes](#remote-nodes)) or [tailcat](#remote-nodes-over-tailcat) when there is no public address or VPN.
- **Adding a machine:** pair it from Devices with a code ([Pairing a separate Node](#pairing-a-separate-node)), or let the Hub [install it over tailcat](#installing-a-node-remotely-over-tailcat).
- **What runs where:**
  - A project's work runs on the Node whose machine has the project.
  - Work without a project stays on the Hub's own Node. `STEVE_NODE_BIND` names another Node for it; without it, a clearly less loaded linked Node takes it (`STEVE_NODE_PLACEMENT=local` turns that off).
  - Tags on Nodes let an item ask for a kind of machine.
  - A remote Node runs only its owner's work, and commands only if its own `policy.json` allows them.
- **When a machine is away:** its sessions wait. After `STEVE_NODE_HANDOFF_MIN` minutes with messages waiting, the item goes on in a new session elsewhere. Work on that machine that was never pushed stays there.
- **Seeing how they do:** Devices shows each machine's load, memory, disk and round trip. It also tells you when a project disk runs short.

## Pairing a separate Node

Remote Nodes are experimental. They run `bin/steve-node`, shipped alongside the Hub and Web in the platform Release bundle. Download the same release for the Node's OS and architecture, verify its manifest, size and SHA256, then extract it into a permanent release directory on that machine. `bin/steve-node --build-info` reports its release version, full commit and target; packaging rejects a Node whose identity differs from the Hub. Source builds remain available for development with `cargo build --locked --release --manifest-path hub-rs/Cargo.toml -p steve-node-cli`.

Before pairing, verify that the Hub has not disabled its default listener with `STEVE_NODE_LISTEN=0` and that its HTTPS reverse proxy forwards `/node/pair` and WebSocket upgrades at `/node/ws` to the Hub. `STEVE_NO_RESUME=1` diagnostic database copies never accept remote Nodes, even when `STEVE_NODE_LISTEN=1` was copied from production. Loopback TCP peers are accepted by default; a proxy on another machine needs its exact TCP peer IP in `STEVE_NODE_PROXY_PEERS` and must overwrite `X-Forwarded-Proto` with its actual incoming scheme. See [Remote Nodes](#remote-nodes) for the HTTPS header requirements and proxy trust boundary. Back up existing configuration, run `steve-hub --check-config` before applying changes, and use the installation's normal restart procedure.

Give the Node its own data directory. Set `STEVE_HOME` explicitly for every Node invocation and in its service environment; never inherit the Hub's data directory. The default, when unset, is `~/.steve-node`. Preserve an existing Node's `node/identity.key`, `node/node.json` and `node/local/` journal when updating it. Do not re-pair an existing identity as a way to reconnect.

The signed-in owner can generate a pairing code in **Devices → Add device** and confirm the pending Node in **Devices**. The authenticated Hub API remains available for the same operations; Agent bearer tokens do not replace the owner session, and API writes require the selected workspace's `x-steve-ws` header.

1. As the signed-in owner, open **Devices → Add device** to generate a pairing code, or call `POST /api/nodes/pairings`. Keep the code private: it expires after ten minutes and can be used once.
2. On the Node, run the following command and enter the code at its stdin prompt. The code is **not** a positional argument. With a publicly trusted HTTPS certificate, or one issued by a private CA installed in the Node's system trust store, no `--pin` is needed. For a pinned certificate, add `--pin sha256:<verified-certificate-DER-hash>`, obtained through a trusted channel.

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

Registration alone does not move existing projects or change scheduling. Do not set `STEVE_NODE_BIND` merely to register a Node. A remote Node runs sessions. It also runs commands (flow steps, vault uses and pushes for projects on that machine) when its owner allows them in `policy.json` (`"exec": true`). Steve's own think and research jobs stay with the Hub.

Where new sessions go:

- `STEVE_NODE_BIND` names one Node for every eligible session.
- `STEVE_NODE_PLACEMENT=spread` (the default) sends a new session (for an item without a project) to the linked remote Node that is clearly less loaded than the Hub's machine. Load per CPU, from the devices' vitals, must be lower by 0.25. Otherwise the session stays on the Hub's machine. `STEVE_NODE_PLACEMENT=local` keeps every such session on the Hub's machine.
- A remote Node runs only its owner's work, whichever way it is chosen.

Every Node can carry tags (Devices → a device → Tags), the Hub's own Node (`local`) included. An item may ask for a tag in several ways:

- `PUT /api/items/<id>/node-tag`;
- the `node_tag` setting of a flow's `item.create`;
- an agent's `steve_devices` tool (`require`), which also lists the devices and their tags.

The item's new sessions then go to a Node with that tag: the Hub's own when it has it, otherwise a linked remote Node that may take the work. When no available Node has the tag, the session says so and is placed as usual.

When a device stays offline for `STEVE_NODE_HANDOFF_MIN` minutes (default 30; `0` turns this off) while messages wait for one of its sessions, Steve ends that session and continues the item in a new one, placed as any new session is. The new session carries the conversation and the waiting messages, and is told that work on the offline device that was never pushed is not visible to it. Steve posts a notice. A device counts as away from when its link closed, and never longer than the Hub has been up.
Any ACP adapter can be a harness. Besides the built-in `claude-code` and `codex`, an entry can declare an adapter of its own: `{"my-agent": {"command": "/absolute/path/to/agent", "name": "My Agent", "args": [], "env": {}, "properties": {}}}`. Only `command` is required; sessions use plain ACP. `properties` declares what plain ACP cannot discover, for example `modes` (Steve's `plan`, `ask`, `accept_edits`, `auto` and `full_access` mapped to the adapter's mode ids; by default `plan`, `default`, `acceptEdits`, `auto` and `bypassPermissions`), `nested_sandbox: false` for an adapter that sandboxes itself and must not run inside the Node's sandbox, `self_read_only`, and `skills` (where under its state directory it reads `<name>/SKILL.md`; see [Skills](#skills)). The Node asks each such adapter `initialize` (at most every five minutes; `probe_initialize: false` turns this off) and reports its version, capabilities and sign-in methods, or why it could not start. The Hub reads the same format from `$STEVE_HOME/harness.json` (or `STEVE_HARNESS_MANIFEST`) for its own Node; an adapter listed there is offered as a bot's harness. `STEVE_ADAPTER_CLAUDE` and `STEVE_ADAPTER_CODEX` still take precedence for the built-in entries, and a manifest that does not parse is logged and ignored.

Registration alone does not move existing projects or change scheduling. Do not set `STEVE_NODE_BIND` merely to register a Node. The current remote Node configuration advertises session capacity; jobs and execs have zero capacity.

### Verify and recover a Node

`steve-node status` reports the saved pairing and fingerprint; it does not establish that the process is online. Verify Devices shows the same Node ID as enabled and connected, or that `GET /api/nodes` reports `state: "active"` and `linked: true`, and check that the Node's own log contains `linked to the hub`. A link attempt that fails for a reason other than the Hub being away (for example `internal`, such as a handshake message over its size limit) is logged by the Node at ERROR as `hub link failed, trying again`, and the Node keeps dialling. The Node notes the last failure in `link-status.json` in its home and removes the file once it links. Devices and `GET /api/nodes` show the last failure as `link_error` (`at`, `error`) while the Node is disconnected, and items whose input waits for that Node say `等待 Node 连接：<reason>`. The Hub reads its own Node's file. A remote Node tells the Hub instead: after such a failure it opens one more connection and answers the Hub's challenge with a signed `report` (its reason, at most 1 KiB) in place of a hello. The Hub checks the signature against the paired key, notes the reason and closes that connection without linking. The Node sends the report again when the reason changes and every 5 minutes. The Hub keeps the reason in memory until the Node links, so after a Hub restart it appears again with the Node's next report. A failure that keeps the Node from reaching the Hub at all (DNS, TCP, TLS, a certificate pin mismatch, a revoked Node) cannot be reported this way, so check the Node's log and `link-status.json` for those. After restarting only the new service, verify the same identity reconnects. Verify execution separately with a bounded test session when an ACP adapter and a project root are configured.

Fingerprints are 32 hex digits (128 bits of the key's SHA-256). Devices shows Nodes paired earlier at the same length, computed from their stored key.

To change a Node's key, for example after its machine or a backup of `node/identity.key` may have been exposed, run `steve-node rotate` on that Node, with its `STEVE_HOME`, then restart its service (`steve node restart`). The new key is kept aside until the Hub has accepted the change, which both keys sign. The old key is kept as `node/identity.key.previous`, and the Hub refuses it from then on and audits the change. A Node must be confirmed (active or paused) to rotate. A Node whose key is lost cannot rotate: revoke it and pair it again.

A Hub that terminates TLS with its own certificate and gives Nodes a pin (`STEVE_NODE_TLS_PIN`) can change that certificate without editing each Node:

1. Set `STEVE_NODE_TLS_PIN` to both the current pin and the next one, comma-separated.
2. Wait until every Node has linked once. The Hub announces its pins in the welcome it signs, and each pinned Node keeps them in its `node.json`.
3. Switch the certificate.
4. Remove the old pin.

`steve setup node --pin` also accepts a comma-separated list.

If a Node's live link is taken over again and again (six times within a minute), two parties are holding the same key. The Hub pauses that Node so that it takes no new work, records `takeover_suspected` in its audit, and posts a notice. Check the machine: if only your own Node runs with that key, rotate it and resume the Node; otherwise revoke it.

The Hub has an identity key, `<hub-home>/secrets/hub-identity.key`, made on first start and included in the [daily backup](#backups). It signs every welcome a Node receives, so a Node knows its own Hub even when TLS ends at a proxy or a tunnel, or the link is plain loopback. Pairing gives the Node the public half (`hub_key` in its `node.json`). A Node paired before the Hub had one remembers it from the first signed welcome. From then on, a Node refuses a Hub that cannot sign with that key, and logs `not the hub this node paired with`. When you move a Hub to another machine, take `secrets/hub-identity.key` with it. If the key is lost, the Hub makes a new one, and each Node must either be paired again or have the `hub_key` line removed from its `node.json` so that it learns the new key; remove the line only after you have made sure the Hub is genuine.

If deployment fails, stop only the newly installed service. Preserve its logs, identity and journal for diagnosis and retry. If abandoning a confirmed registration, the owner can call `POST /api/nodes/<node_id>/revoke`; a revoked identity cannot reconnect. The local `forget` command only removes pairing files and does not revoke access at the Hub. Restore any backed-up Hub configuration through the same controlled restart procedure if enabling the listener must be rolled back.

## Hub and Node service commands

The native `steve` CLI manages user launchd jobs on macOS and user systemd units on Linux. A verified installer archive provides the same executable at `<installer>/bin/steve` when Homebrew is not used. Use a stable, verified extracted bundle and explicit data directories; Hub and Node must have separate homes. Linux requires a working user systemd session. macOS LaunchAgents run in the signed-in user's session. A new macOS Hub requires the explicit first-time Keychain setup below before `hub install`; the installer will refuse an unsigned home instead of silently selecting another vault backend.

```sh
steve hub install --home "$HOME/.steve" --release <bundle>
steve hub start --home "$HOME/.steve"
steve hub status --home "$HOME/.steve"
steve hub doctor --home "$HOME/.steve"

steve node install --home "$HOME/.steve-node" --release <bundle>
steve node start --home "$HOME/.steve-node"
steve node doctor --home "$HOME/.steve-node"
```

Both components also support `stop` and `restart`; these commands work without a reachable Hub. `install` is idempotent and does not start the service, create a pairing, change roots or permissions, install a keyhelper, or rewrite `steve.env`. Reinstalling the same configuration leaves a running process alone. `--label` selects a custom service label at initial installation; signed Hub installations retain the label and launch mode in `signing/identity.json`. Changed service definitions have a `.previous` backup. Service metadata is under `<home>/services/`; macOS logs are `<home>/logs/<component>.log`, and Linux logs are in `journalctl --user -u <label>`.

Hub and Node service commands print a readable summary when stdout is a terminal. Add `--json` for the existing machine-readable report; stdout redirected to a file or pipe also keeps JSON output. These choices do not change the service action or its exit status. `steve --build-info` always returns JSON, `steve --version` reports the installed CLI/package version, and `steve upgrade watch` keeps its existing streaming output.

The report separates the service manager's observed state and PID from the configured release/build and any verified runtime identity. An unverified runtime identity means the CLI has not probed it; it is not proof of a failed connection. `start`/`restart` report the service-manager result, not a completed browser login. `doctor` checks the local installation, service definition and Node configuration; it does not test HTTP health, a public reverse proxy, OIDC discovery or model authentication. Check Hub `/healthz` for the actual running Hub/Web build and test browser login separately. A running Node process and a saved pairing do not establish that it is connected or has no active sessions; verify those in Hub Devices. For installations previously managed by another script, pass their existing label and adopt the same release while stopped; the tool does not discover or terminate arbitrary foreground processes.

To update a separate Node, verify and extract the new bundle, inspect the candidate, finish its sessions in Steve and stop its service, then switch its service definition:

```sh
steve node doctor --home "$HOME/.steve-node" --release <new-bundle>
steve node stop --home "$HOME/.steve-node"
steve node update --home "$HOME/.steve-node" --release <new-bundle>
steve node start --home "$HOME/.steve-node"
```

An update refuses a running or unknown service state before changing files. Optional `--wait <seconds>` waits only for the service to stop; it does not drain sessions, stop the service or kill a process. The executable path is pinned to the selected release, and `node/identity.key`, pairing, journals, `roots.json`, `policy.json` and `harness.json` remain in the same home. Keep the previous bundle. Selecting it uses the same stopped-service `update` procedure, but an older Node may reject a newer journal format; verify compatibility before rollback and verify the original fingerprint reconnects. Use **Settings → About** for Hub upgrades: direct service replacement would skip its drain, database backup and migration checks.

## Source-based macOS service installation

Copy `ops/steve.env.example` to `~/.steve/steve.env` and adjust it to your deployment. Keep secrets outside this checkout, using the supported `*_FILE` configuration. On macOS the vault master keys live in the login keychain and only `~/.steve/bin/steve-keyhelper` reads or writes them; `ops/release.sh` rebuilds it only when its own source changes, so rebuilt Hubs keep access to the keys. Run `zsh ops/install.sh` from the checkout to build a source release and install user launchd jobs. Logs are under `~/.steve/logs`.

`ops/release.sh` builds from `main` and switches source releases after checks. `STEVE_REPO`, `STEVE_HOME`, `STEVE_LABEL` and `STEVE_BACKUP_LABEL` support custom locations and service labels. Existing installations must preserve their original signing identities and keychain service: set `STEVE_KEYHELPER_SERVICE`, `STEVE_KEYHELPER_HUB_IDENTIFIER`, `STEVE_KEYHELPER_IDENTIFIER` and `STEVE_LABEL` to the original values in the build environment (they are compile-time identities, not `steve.env` settings) and sign with the original certificate. Changing a namespace or vault backend does not migrate keys. Back up data and verify recovery material before changes.

Existing signed macOS installations can upgrade from **Settings → About**, or the [local upgrade CLI](#local-upgrade-cli) for the account's default home, using the local release runner described below. Their original signing identity and installed keyhelper are retained; no vault migration is needed.

## Managed Release bundles and automatic upgrades

Automatic upgrades require a published bundle matching your platform and either Steve's supervisor or the signed macOS local release runner. Release builds target Apple Silicon macOS 14+ and Linux x86_64 with glibc 2.35+ (Ubuntu 22.04+ or Debian 12+); release-asset availability determines which platform can actually upgrade. Windows is not supported. Source development and externally managed containers expose checks only.

Initial installation uses a verified bundle:

1. Select a stable release from [Infranite/steve Releases](https://github.com/Infranite/steve/releases) with a bundle for your OS and architecture. Verify its manifest and SHA256 before extracting it. If no matching release asset has been published, this installation path is not available yet.
2. Install the extracted, paired Hub/Web bundle under `$STEVE_HOME/releases/<version>` and use the service `install --release` command to create `$STEVE_HOME/current`. A bundle contains `bin/steve-hub`, `bin/steve-node`, `bin/steve-keyhelper`, `web/dist`, `release.json` and the runtime `ops/` scripts; do not substitute a source checkout or mix separate frontend/backend builds.
3. Configure `$STEVE_HOME/steve.env`, authentication, the optional public URL, and a supported persistent vault backend. New Linux installations can explicitly use `STEVE_VAULT_KEYSTORE=dir` with `STEVE_VAULT_KEY_DIR` outside `STEVE_HOME`, on protected storage owned by the Hub user with mode 0700, kept mounted across releases. The `file` backend is for isolated development and tests only. Existing macOS Keychain/helper installations use the local release runner below, which signs the downloaded Hub with the original certificate. Do not change their vault backend or namespace.
4. Run `steve hub install --home "$STEVE_HOME" --release <bundle>`, then `hub start --home "$STEVE_HOME"`. New managed installations run `ops/run-managed.sh` as the foreground service process (`steve-hub --supervise`). An existing signed installation keeps `ops/run.sh` and its original launchd label. Use `hub doctor` and `/healthz` to verify startup.


For a new macOS home, extract the verified bundle under `$STEVE_HOME/releases/<version>` and run this once before step 4:

```sh
"$HOME/.steve/releases/<version>/bin/steve-hub" --local-release \
  setup-keychain "$HOME/.steve/releases/<version>" --home "$HOME/.steve"
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

The supervisor also asks the running Hub's `/healthz` every 30 seconds. A Hub that has answered once since it started and then misses four probes in a row is taken for hung: it is stopped, and the supervisor exits, so the service manager starts both again, the same as when the Hub exits on its own. No probe counts while an upgrade is in progress.

In Settings -> About -> Versions, check for updates and review the release notes. Only the instance owner can execute an upgrade or approve/reject an Agent request. Agents use `steve_upgrade` to check status. Upgrade requests are available only in a workspace whose sole member is the instance owner; shared-workspace agents cannot request instance upgrades. Workspace ownership alone does not grant upgrade permission.

An upgrade downloads and validates the bundle, waits for active work to drain, stops the old Hub, backs up SQLite, then verifies the candidate before committing the version switch. Failures before commit restore the old bundle and database; failures after commit retain the new database to avoid discarding resumed work or external effects. The UI reconnects without submitting the upgrade again. Completed upgrades leave the current page open until you choose to reload; persisted conversation drafts are retained.

The Hub checks for a newer release shortly after it starts and then every six hours (`STEVE_UPDATE_CHECK=0` turns this off; debug builds only check with `STEVE_UPDATE_CHECK=1`). When it finds one, the sidebar and Settings show a notice, and the owner sees it once as a toast. Only the owner can start a check from the page.

A signed macOS installation uses the same published Hub/Web bundle without compiling locally. The Hub downloads the matching archive and verifies its size and SHA256 against the release manifest before safe extraction. `local-release.py prepare-bundle` verifies the manifest, Hub build and Web identity, then signs only the candidate Hub with the existing certificate and Hub identifier. It never overwrites `$STEVE_HOME/bin/steve-keyhelper`; the bundled generic helper is not installed during an upgrade. The deployment probe must read the existing vault before switching the service. A missing signing identity, helper or keychain stops the upgrade instead of changing the vault backend or namespace.

The signed automatic runner grants Keychain access only for the account's default `~/.steve` home. A custom macOS Hub home can still use the service commands, but upgrading it requires an explicit terminal invocation of `local-release.py prepare-bundle <verified-extracted-bundle> --version <version> --commit <full-sha>`, followed by `deploy <bundle>`, with `STEVE_HOME` set to that home and its original signing identity; this may show a one-time macOS Keychain prompt. An installation whose vault keys are in a directory of their own (`--vault-dir`) keeps the signing keychain's password there too: the two commands read it from that directory, show no prompt and never open the login keychain's entry, which is the usual installation's. `hub doctor` reports this limitation. Linux managed installations and separate Nodes support custom homes normally.

This path needs Python 3.9+, `codesign`, the original `signing/identity.json` and signing keychain. It does not require a checkout, Cargo, npm or Git. The signed Hub passes the signing password to the installer through stdin; it is never an argument or a log value. The signing keychain is unlocked only while codesign runs and locked again afterwards. Use the authenticated Settings page or the local CLI below. Explicit developer source builds remain available as `local-release.py build <commit> --version <version> --from-source --repo <checkout>` and require Cargo, npm, Git and Python 3.12+.

For the first upgrade from an older local runner, its legacy `build` call now downloads and prepares the release bundle by default; it does not compile. That older runner still needs Git and a Python with `tarfile.data_filter` to read the new tag's scripts once. The CLI supplies an isolated bare object cache; the older Settings path uses its configured checkout. Subsequent upgrades use the bundle path directly. A failed download or integrity check never falls back to an unrequested source build.

The operation journal records download bytes, preparation, the actual drain snapshot, backup, probe, restart and health verification. Waiting for active work is a separate phase, not a simulated percentage. Its log is `$STEVE_HOME/logs/release/upgrade-<version>-<id>.log`. The native installer, runner and legacy maintenance scripts share the POSIX record lock `releases/.release.lock`, so only one release can switch the installation at a time.

Eligible interrupted work resumes through task recovery. Paused work and pending approvals remain waiting; uncertain external delivery outcomes require reconciliation. Keep `$STEVE_HOME/updates` journals, backups and old versions until you accept the upgrade. After a healthy local deployment, pruning keeps the current and previous releases plus the newest three recognized release directories and eight pre-deployment database backups. UUID-named bundle directories are eligible only when their manifest identifies the same version; unrelated directories are not pruned.

### Local upgrade CLI

For a signed macOS Hub installed at the current OS account's default `~/.steve`, run the bundled CLI as that account, without `sudo`:

```sh
steve upgrade check
steve upgrade check --version X.Y.Z
steve upgrade request --version X.Y.Z --key upgrade-X.Y.Z
steve upgrade watch --timeout 600
steve upgrade status --operation <operation-uuid>
```

Replace `X.Y.Z` with the published stable version selected by `check`. The request returns a durable operation ID; submission is not completion. `watch` emits JSON when the journal changes and exits 0 only for `succeeded`, 1 for a failed or rolled-back operation, and 2 when its time limit expires. Closing the terminal does not stop launchd's runner. Repeat the same key to retrieve the same operation; a conflicting version or actor is refused. A lost runner is recorded as failed after its startup grace period, without repeating possibly executed work. Inspect the recorded log, current version and backup before using a new key.

The CLI derives the account and home from the OS user database, verifies the installed release and signature identifiers, and records `local-os:<uid>` as the requester and approver. It does not read or export a signing password, browser cookie or Keychain item. Only the existing signed Hub executes `--upgrade-runner`. The journal, runner and release locks are shared with the Settings path. Existing approvals, live runners and untrusted legacy candidates are never replaced by a new request.

Release repository and port come from the installed service definition, overridden by literal `STEVE_UPDATE_REPOSITORY` and `STEVE_PORT` assignments in `steve.env`, matching `run.sh`. The CLI does not source that file; duplicates, expansions and shell expressions in these settings are refused. Defaults are `Infranite/steve` and port 8440. Caller environment overrides, Git configuration and credentials are not forwarded. Both the CLI and signed runner verify the published release identity; the runner verifies and prepares the actual bundle.

New Hubs advertise `upgrade_runner.native=true` in `--build-info`. The native CLI verifies this capability before passing an internal command to the signed Hub, then uses `release_bundle` without Python, Git or a checkout. For an already-running older Hub, use its existing Settings upgrade path for the first transition: its old runner may still use its already-installed Python once. Runtime archives retain those legacy scripts and their frozen extraction contract; new native installer archives omit the Python scripts. The running new Hub and all subsequent native upgrades do not invoke them. Unknown capabilities are refused rather than guessed from a version number. Linux managed Hubs and custom macOS homes retain their existing upgrade paths; native `steve hub` and `steve node` lifecycle commands work on both platforms.

## Remote Nodes

The listener is enabled by default; `STEVE_NODE_LISTEN=0` disables it. The owner can use Devices → Add device to generate a ten-minute, one-use pairing code. On the target machine, run `steve-node pair https://your-hub.example --name my-device` and enter the code through standard input. Compare the fingerprint printed by the CLI with the pending device in the owner page, then confirm it before starting the Node service. Pairing does not grant access to any project directories; configure roots separately. `STEVE_NODE_BIND` has no default: assigning new eligible sessions to a paired Node remains an explicit operator choice. Same-principal workflow children are eligible under these same rules; pairing does not authorize another principal’s work.

The `/node/pair` and `/node/ws` routes accept only loopback TCP peers by default, for a TLS proxy on the Hub machine. If TLS terminates on a different host, set `STEVE_NODE_PROXY_PEERS` to that proxy's exact IP address as seen by the Hub (comma-separated for multiple proxies). Hostnames, CIDRs, wildcards and unspecified addresses are rejected by `steve-hub --check-config` before deployment. An unset or empty value preserves loopback-only behavior.

Only the actual TCP peer is trusted; `X-Forwarded-For` never grants access. A configured non-loopback proxy must overwrite `X-Forwarded-Proto` with its actual incoming scheme. The Node routes require exactly one value, `https`; missing, duplicate and comma-joined values are refused. Protect the proxy-to-Hub hop with a private encrypted network or tunnel, restrict who can originate from the configured peer, and never copy a caller-provided scheme header. This configuration does not replace pairing, signature verification, owner confirmation or per-Node authorization.

### Remote Nodes over tailcat

When a Node cannot reach the Hub over HTTPS (no public address, no VPN), the Hub can be reached over [tailcat](https://github.com/tailscale/tailcat), an optional program from Tailscale that tunnels TCP between two machines without an account or root. Install `tailcat` on both machines (`STEVE_TAILCAT` names the program when it is not on the service's `PATH`). The owner then turns on **Devices → Add device → Connect over tailcat**.

The Hub then:

- makes a key once in `<hub-home>/tailcat/` so that its address stays the same across restarts;
- listens on `127.0.0.1:<port>` with the Node routes (`/node/pair`, `/node/ws`) and nothing else; the port is `STEVE_TAILCAT_PORT`, by default the Hub's port + 2;
- runs `tailcat serve <port>` as its own child, starting it again if it exits and ending it when the Hub stops.

Devices shows the resulting address, `tailcat:<address>:<port>`. Use it wherever an HTTPS Hub URL goes: `steve setup node --hub tailcat:<address>:<port>`, or `steve-node pair tailcat:<address>:<port>`. The Node runs `tailcat forward` itself and dials its loopback end. The tunnel is WireGuard between the two machines, and the address names the Hub's key, so no certificate or pin is involved. Treat the address like a credential: it does not grant access by itself, because pairing still needs a one-time code and the fingerprint is still confirmed, but it is what lets a machine try. Turning it off stops the listener and the tunnel; paired Nodes reconnect when it is on again.

### Installing a Node remotely over tailcat

From **Devices → Add device → Install remotely over tailcat**, the Hub can install a Node on another machine itself. Both machines need `tailcat`; the Hub also needs OpenSSH's `ssh` and `ssh-keygen`.

1. Devices shows one line for the target, carrying a key made for this install only:

   ```sh
   tailcat serve --ssh-authorized-keys='ssh-ed25519 AAAA… steve-install' ssh
   ```

   Run it on the target as the user the Node should run as; no root is needed. It prints an address.
2. Paste the address into Devices. The Hub then works through steps the page shows:
   1. reads the target's platform with `uname`;
   2. fetches the installer archive of its own version for that platform and checks it against the release's `steve-installer.json` (size and SHA256);
   3. sends it over the tunnel and unpacks it in `~/.steve-node-install`;
   4. runs `steve setup node --hub <this Hub>` and `steve node start`.

   The pairing code goes over stdin, never on a command line. A code made for an install is *confirmed*: the owner authorised the install, so the Node is active once it pairs, with no fingerprint to compare by hand.
3. When it is done, stop `tailcat serve` on the target. The key is deleted on the Hub, and the unpacked archive on the target.

The new Node reaches the Hub over tailcat when [Connect over tailcat](#remote-nodes-over-tailcat) is on, else at the Hub's HTTPS `STEVE_PUBLIC_URL`. Without either, the install refuses to start. A development build has no release to install from. A Hub without access to GitHub can take the release files from a local directory with `STEVE_NODE_INSTALLERS=<dir>` (`steve-installer.json` and the installer archives of its version). Supported targets are those of the releases: Apple silicon macOS and x86_64 Linux. Without tailcat, use the paste-in command Devices shows with the pairing code.

## The Hub's own Node in a process of its own

The Hub's own Node, which runs every session on the Hub's machine, runs as `steve-hub --local-node`, a process of its own (`STEVE_NODE_MODE=service`, the default), so a turn goes on while the Hub restarts or upgrades. `STEVE_NODE_MODE=local` keeps it in the Hub's process instead, where a Hub restart cuts the turns that are running:

- The Hub starts the process when no Node is linked: in a session of its own and, when the Hub runs under systemd, in a scope of its own (`systemd-run --user --scope`). A restart or upgrade of the Hub's service therefore leaves it running. `steve hub stop` (and so the stop before `steve hub uninstall`) stops it too.
- It keeps the Node's id (`local`) and home (`<data>/node`), so sessions from before carry on there.
- It links to the Hub on `127.0.0.1:<STEVE_LOCAL_NODE_PORT>` (default: Hub port + 3), whatever addresses the Hub's own port listens on. It checks the Hub's signed welcome. The hello stays bounded as stream history grows; the Hub reconciles stream positions from snapshot pages after linking.
- The Hub registers it as its own paired Node, with a key in the Node's home (`service-identity.key`).
- Its roots follow the Hub's projects through `<data>/node/host-roots.json`.
- Its log is `<data>/node/logs/local-node.log`, and its pid is in `<data>/node/service.pid`.
- Steve's own think and research jobs stay in the Hub's process, under `<data>/node-jobs`. Research is read-only, with one exception: in the instance owner's own main conversation Steve works with the full access of the OS user the Hub runs as (files, commands, network), starting in that user's home directory. Steve's data directory (keys, database, programs) stays out of its reach there too. On an instance with a single user, the owner's private chat with Steve in a channel works the same way. Other members' conversations, shared conversations in channels and research on a remote Node stay read-only.
- An upgrade's drain does not wait for this Node's turns. Once the Node has no turn running, a Node from another build is restarted onto the Hub's build.
- The remote Node listener must be on (the default; `STEVE_NODE_LISTEN=0` and `service` together refuse to start).
- Going back to `local` is an ordinary restart with `STEVE_NODE_MODE=local`. Stop it first with `steve hub stop` (or `kill $(cat <data>/node/service.pid)` for a Hub not set up by `steve setup`) so that two Nodes never use one home.

## Harness setup

### The ACP adapters

The built-in harnesses run through `@agentclientprotocol/claude-agent-acp` (Claude Code) and `@agentclientprotocol/codex-acp` (Codex). Both are Node.js programs, so a machine that runs sessions needs Node.js; the Homebrew formula depends on it. `steve setup` installs the ones you choose with npm into `<home>/adapters` (a prefix of their own, not the global one): tick them in the wizard, or pass `--adapters claude-code,codex`, `--adapters all` or `--adapters none`. `steve adapters install <id>…` adds one later. Without npm, setup goes on and says to run `steve adapters install` once Node.js is there. No version is pinned: Steve runs their latest release. An adapter you did not choose is not installed or upgraded behind your back, and the startup check does not report it as missing.

A Node asks the npm registry (as npm is configured, so a mirror works) a minute after it starts and then once a day. With `STEVE_ADAPTER_UPDATES=auto` (the default) it installs a newer release when it has no session or job running, and the next session uses it; `check` only shows it; `off` asks nothing. **Devices → runtimes** shows the installed and latest release for each. By hand:

```sh
steve adapters status            # installed and latest
steve adapters install codex     # add one that was not chosen at setup
steve adapters upgrade           # install the latest now
steve adapters status node       # a separate Node's (its home: ~/.steve-node)
```

Adapters named by `STEVE_ADAPTER_CLAUDE` / `STEVE_ADAPTER_CODEX` or by `harness.json`, or installed globally, are used as they are and left alone.

Run `steve-hub --check-config` in the Hub service's environment before installation or an upgrade. Missing local ACP adapters and Node.js are reported as setup warnings with the setting to fix; the Hub can still start so its settings and diagnostics remain usable. `steve setup` installs the adapters into `<home>/adapters` (Node.js required); `steve adapters status` shows them and `steve adapters upgrade` installs the latest. `STEVE_ADAPTER_CODEX` / `STEVE_ADAPTER_CLAUDE` point at adapters installed elsewhere. For JavaScript adapters, set `STEVE_NODE` to the Node.js executable when it is absent from the service's `PATH`. A separate Node uses its own `harness.json` and installed adapters; a successful Hub preflight does not certify a remote machine's environment.

`STEVE_PRINCIPAL_STATE` now defaults to `on` on the Hub. New sessions use their principal's own harness state; existing sessions retain their recorded state. The Node imports only the owner's adapter-declared settings (`config.toml` for Codex, `settings.json` for Claude Code). Codex login is never copied, and other principals begin without the owner's settings or login. The preflight and normal startup log explicitly flag this migration; Steve does not read login contents or certify authentication by checking for a file.

**Principal authentication setup:** Steve does not yet manage harness login from the Web UI or Node CLI; default-on principal state therefore still requires an operator to run the harness's login command on the target Node, and you must not assume the Node service account's existing Codex login applies. The ordinary session resume action (`GET /api/sessions/<session-id>/resume`) uses the configured harness home and is not a way to discover an independent principal state directory.

To inspect a session's recorded principal state while the Hub is running, pause the target session and call `GET /api/sessions/<session-id>/principal-state` authenticated as the instance owner in that session's workspace (`x-steve-ws`). It returns `bound`, `node_id`, `principal`, `harness`, `independent`, `state_dir`, `harness_rel_dir` (`state/<state_dir>/harness`) and, when `node_id` is `"local"`, `local_harness_dir` (`<configured Hub data directory>/node/state/<state_dir>/harness`; `null` for a separate Node, whose directory is `<its STEVE_HOME>/node/local/<harness_rel_dir>`). If `independent` is `false` (`state_dir` is `null`), the session has no recorded independent directory: stop here rather than guessing a hashed path or fabricating a binding.

Only as an offline fallback when the Hub is stopped, an operator authorized to access the Hub database can inspect the same binding in read-only mode:

```sh
sqlite3 -readonly "<configured Hub data directory>/steve.sqlite3" \
  "SELECT session_id, node_id, state_dir FROM runtime_sessions WHERE session_id = '<session-id>';"
```

Do not update the database. If there is no row or `state_dir` is empty, stop rather than guessing a hashed path. Match `node_id` to the selected Node (`<STEVE_HOME>/data/node` for `local`, `<its STEVE_HOME>/node/local` for a separate Node) and use `<Node journal directory>/state/<recorded state_dir>/harness` without reconstructing its segments.

On that Node, authenticate the intended principal with the harness's normal login command and this directory. For Codex, run `CODEX_HOME="<absolute recorded harness directory>" codex login` as the Node service account, using that principal's own authentication, then verify authentication before retrying the paused session. Do not copy another installation's `auth.json` or refresh tokens. If the operator cannot complete this procedure, authentication setup remains a blocker; this release does not replace it with an automatic login import. `STEVE_PRINCIPAL_STATE=off` remains an explicit shared-state configuration for new sessions only; it does not migrate existing session state.

`STEVE_TOOLCHAINS` defaults to `on` on each Node. Detected build outputs go into the item's `.steve/build/` and cache-only directories go into the known principal's Node cache; the first build after relocation may be cold. A session without a principal receives no invented cache or identity. Set `STEVE_TOOLCHAINS=off` on that Node to retain the original toolchain locations. Toolchain relocation and separate harness directories are not claims of complete principal isolation: Codex retains its own sandbox and approval controls, Linux outer confinement is on by default where bubblewrap is installed, and the existing remote Node single-owner rule and workflow same-principal admission still apply.

### Capabilities: MCP servers, skills and remote agents

**能力 · Capabilities** in the sidebar is the workspace's library of what bots can use: MCP servers, skills and remote A2A agents. Each is defined once there and chosen per bot on the bot's own **Capabilities** tab, which also has a switch per entry for that bot alone and **Add** (pick one from the library, or make a new one, which goes into the library and to that bot at once). Editing an entry changes it for every bot that uses it; taking it off a bot leaves it in the library; deleting it takes it off every bot. The library's lists say which bots use each entry, and an entry's bots can be chosen from there too.

The API:

- `GET|POST /api/capabilities/mcp-servers`, `PATCH|DELETE /api/capabilities/mcp-servers/{id}`
- `GET|POST /api/capabilities/skills`, `PATCH|DELETE /api/capabilities/skills/{id}`
- `GET|POST /api/capabilities/a2a-agents`, `PATCH|DELETE /api/capabilities/a2a-agents/{id}`, `POST /api/capabilities/a2a-agents/preview {"card_url"}` (reads a card without keeping it)
- `GET /api/bots/{id}/capabilities` → `{mcp, skills, a2a}`, each entry with `enabled` for that bot
- `PUT /api/bots/{id}/capabilities/{mcp|skill|a2a}/{entry} {"enabled": true|false}` assigns or switches one; `DELETE` the same path takes it off the bot

Library answers carry `bots: [{id, enabled}]`. A `POST` with `"bot": "<id>"` also assigns the new entry to that bot. A bot uses at most 32 entries of each kind, and the names its harness sees stay distinct among them. Every change is in the audit log by name (`mcp_server.*`, `skill.*`, `a2a_agent.*`, `bot_capability.assign|enable|disable|unassign`). Migration 65 moved the per-bot servers and skills of migrations 62 and 63 into the library, assigned to the bots that had them; a name two bots of one workspace both used gets `-2`, `-3`, … on the later ones.

#### MCP servers

A server has a name (`[A-Za-z0-9_-]`, unique per workspace in any case, not `steve`) and a transport: `stdio` with `command`, `args` and `env`, or `http` / `sse` with `url` and `headers`. Each variable or header is `{"name","value"}` or, for a secret, `{"name","value","secret":true}`.

When a session of a bot opens (or reopens after it was released), the Hub puts the servers the bot has switched on into the Node's `open_session` command. The Node passes them to the adapter in ACP `session/new` / `session/load` (`mcpServers`) after Steve's own `steve` server, so every harness gets them the same way. A change applies the next time a session's harness opens. A `stdio` command runs on the session's Node, inside the same sandbox as the harness; install what it needs there.

Secret values go into the workspace's system vault (one credential item per server, fields `env:<name>` and `header:<name>`). The server row, API answers, the audit log, the Hub's command ledger and the Node journal hold only the name and a reference. The Node takes each value from the Hub under `mcp:<session>` each time the harness opens; the Hub answers only the Node the session is bound to, and only for a server the session's bot has switched on. Each use appears in the vault's use log. Saving a server again without a secret's value keeps the stored one; deleting the server destroys its credential item. A server whose secret cannot be taken (no vault, a Node from before this release) is left out of that session, with a warning in the Node log, and the session opens without it. An older Node ignores the servers altogether.

#### Skills

A skill is a `SKILL.md`: a `name` (lower-case letters, digits and single hyphens, at most 64, unique per workspace), a one-line `description` of when to use it (at most 1024 characters) and Markdown `content` (at most 32 KiB). A bot's switched-on skills may hold at most 128 KiB together; switching one on, or growing a skill some bot has on, past that is refused. The page can fill a new skill from a pasted `SKILL.md`; front matter other than `name` and `description` is not kept.

Whether a harness reads skills, and where, is declared by its adapter (the `skills` property: a directory under the harness's own state directory). The built-in Claude Code and Codex adapters declare `skills`, so a bot's skills go to `$CLAUDE_CONFIG_DIR/skills/<name>/SKILL.md` and `$CODEX_HOME/skills/<name>/SKILL.md`, where each harness reads its user skills. An adapter in the harness manifest declares its own (`"properties": {"skills": "skills"}`) or none; for one without it, the page says the harness does not read skills and the skills are kept unused.

When a session of a bot opens (or reopens), the Hub puts the skills the bot has switched on into the Node's `open_session` command, and the Node writes them into the session's own harness directory (`state/<state_dir>/harness`, see the principal harness state above) before ACP `session/new` / `session/load`. The Node records the skills it wrote in `.steve-skills.json` beside them and keeps that set exactly the session's: a changed skill is rewritten, a removed or switched-off one loses its `SKILL.md` (its directory goes only when nothing else is in it), and a skill the harness or a person put there is left alone. A change applies the next time a session's harness opens. A session without a harness directory of its own (`STEVE_PRINCIPAL_STATE=off`, or an adapter that cannot move its state) gets no skills; the Node logs that. An older Node ignores them.

#### Remote agents (A2A)

A remote agent is added by the address of its [A2A](https://a2a-protocol.org) Agent Card. An address that names only a host is looked up at the standard `/.well-known/agent-card.json`, then the older `/.well-known/agent.json`. The Hub reads the card (at most 512 KiB; it needs a `name` and the agent's `url`) and the page shows its name, description, version and skills before it is kept. **Read the Agent Card again** fetches it anew. If the agent wants a key, choose `bearer` (sent as `Authorization: Bearer <key>`) or a header of your own (`<header>: <key>`); the key goes into the vault (field `auth` of the agent's credential item) and is shown only as `has_secret`.

A bot that uses a remote agent gets one Steve tool for it, `steve_a2a_<name>` (the name defaults to one made from the card's, lower-case letters, digits and `_`). Its description is the card's description and skills. It takes `message`, and optionally `context_id` and `task_id` from an earlier answer: the Hub sends A2A `message/send` (JSON-RPC, blocking) to the agent's `url`, asks again with `tasks/get` while the task is `submitted` or `working`, and answers with what the agent said, its artifacts, and its task and context ids. A task still running after 100 seconds (a Node gives a Steve tool call two minutes) is answered with its `task_id`; calling the tool again with only that id gets the result later. When the agent waits for input (`input-required`), the answer says so; the bot replies with the same tool and `task_id`. The tool is Steve's, so every harness gets it the same way; it is listed in a session's tools from the next list the harness asks for. Each use of the key is in the vault's use log. Streaming and push notifications are not used.

#### Steve as an agent

Steve publishes its own Agent Card at `/.well-known/agent-card.json` (and `/.well-known/agent.json`) on the Hub's address, without sign-in. Its `url` is `<the Hub's address>/a2a`, the address taken from the configuration (`STEVE_PUBLIC_URL`, `STEVE_PUBLIC_URLS`, else the local network's, else the one the request came by). **Capabilities · A2A** shows both addresses. `POST /a2a` takes JSON-RPC 2.0 with one of the user's access tokens (`Authorization: Bearer stt_…`; `x-steve-ws` names a workspace when the user has several):

- `message/send`: a message without `taskId` becomes a new item the user asked for (source `A2A · <token name>`), its bot and title chosen as for a message; with the `taskId` of one of the user's items it adds the text to that item. The answer is the task at once; its `id` and `contextId` are the item's id.
- `tasks/get {"id"}`: the item's state (`submitted` before its first session, then `working`; `input-required` while a question, approval or acceptance is open for the user; `completed`, `canceled` or `failed` when it ended), what Steve reads of it as the status message, and its last delivery as an artifact once it is done.
- `tasks/cancel {"id"}`: drops the item.
- `message/stream`, `tasks/resubscribe` and push notification settings are answered as not supported.

#### Steve's MCP for clients outside Steve

Steve's own tools are an MCP server too: every bot's session has it ("Steve · built-in" on the Capabilities pages, always on, its tools folded inside with what each is for). A client outside Steve – Claude Desktop, Cursor, another agent – can use the same endpoint, `/mcp` on the Hub's port, with one of its user's access tokens (see [Access tokens and clients](#access-tokens-and-clients)) sent as `Authorization: Bearer stt_…`. Unlike a Steve session's own token, which works only from the Hub's machine, an access token is accepted from any address the Hub answers. The client acts as that user in their first workspace; `?ws=<id>` on the URL or an `x-steve-ws` header names another. A revoked or expired token is refused at once.

Such a client gets only the tools that need no Steve session:

| Tool | What it does |
| --- | --- |
| `steve_items_search`, `steve_item_read` | find the workspace's items and read one (goal, status, plan, timeline, last delivery) |
| `steve_item_create` | hand Steve new work: `goal`, and optionally `title` and `bot` (id or name); without them Steve chooses as for a message. The item's source is `MCP · <token name>` |
| `steve_item_supplement` | add to an item, or answer what its bot asked (`id`, `text`) |
| `steve_knowledge_search`, `steve_knowledge_read` | the user's knowledge base |
| `steve_memory_list` | the user's memories (read only; tidying them stays with Steve's own runs) |
| `steve_sessions_list` | the agent sessions and what they are doing |

Everything that belongs to a session's own turn (`steve_update`, `steve_ask`, `steve_deliver`, `steve_publish`, the vault, GitHub, mail, workflows, …) is neither listed nor run for it. An outside agent that needs the owner's input creates an item or adds to one: the owner sees it in Steve like any other.

**Capabilities · MCP servers · Connect to Steve from outside** shows the endpoint for each address the Hub is configured to be reached by (public first, then the local network, then the machine's own), makes an access token (shown once), and gives the configuration for the client with the token filled in:

```json
{ "mcpServers": { "steve": { "type": "http", "url": "https://<hub>/mcp", "headers": { "Authorization": "Bearer stt_…" } } } }
```

Cursor takes the same without `type` in `~/.cursor/mcp.json`. Claude Desktop starts local commands from `claude_desktop_config.json`, so it reaches the Hub through `mcp-remote` (Node.js): `{"command": "npx", "args": ["-y", "mcp-remote", "https://<hub>/mcp", "--header", "Authorization:${STEVE_AUTH}"], "env": {"STEVE_AUTH": "Bearer stt_…"}}`. Claude Code: `claude mcp add --transport http steve https://<hub>/mcp --header "Authorization: Bearer stt_…"`. A client on another machine needs the Hub to listen beyond loopback, or a public address with HTTPS from your own reverse proxy (see the setup above). `GET /api/capabilities/steve` answers the same lists and addresses: `{tools, outside, endpoints: [{url, kind}], agent_card, a2a}`.

### Permission modes and full access

A session opens in its bot's permission mode (`default` widened to `acceptEdits` for an item with its own directory); a session without a bot opens in the harness's asking mode. 完全放开 (full access) asks the harness for its no-prompt mode; the Node switches a running session at its next idle boundary, and a Node owner's ceiling can refuse it. Leaving full access asks for the bot's mode again, or the asking mode for a session without a bot. After a Hub restart, a harness the Node still runs in a wider mode than Steve wants is switched back at the next check.

`GET /api/yolo/items/<item-id>` (in the item's workspace, `x-steve-ws`) shows what applies: `level` and the layer that set it (`source`: item, project or workspace), and per session `wanted` (the mode Steve asks for), `wanted_from` (`full_access`, `bot`, `bot_bypass`, `node_default` for a session without a bot, or `readonly`), `applied` (the mode the Node confirmed the harness runs in), `desired` and `status`. `status` is `applied` only once the Node confirmed the wanted mode; otherwise it is `after_turn`, `sending`, `next_open`, `not_sent`, `refused` (with the Node's or harness's reason in `refused`), `unknown` or `not_running`.

While a turn still runs in its old mode, YOLO answers the requests its harness asks within full access's limits. It judges every command a line runs (shell `-c` and here-documents, `eval`, `find -exec`, `xargs`, wrappers such as `env -S`, `watch`, `setsid` or `flock`, git's `-x`/`bisect run`/aliases, sed scripts and inline interpreter code) against the same hard limits as the line itself. Commands carried in variables (`GIT_EDITOR`, `PAGER`…) and options (`git grep -O`, `fd -x`, `tar --to-command`), and those package and environment managers run (`npm exec`, `bundle exec`, `uv run`, `direnv exec`…), are judged the same way. This is a static reading of the text, not a sandbox: what it cannot read, such as a script piped into a shell, inline code that can start programs or evaluate text, a variable that loads code (`LD_PRELOAD`, `NODE_OPTIONS`), a sed script that executes, or a wrapper option it does not know, is left to the user. The limits only see what the command line says: a program or script in a file, including one the agent has just written, runs as the program it is, and what it does inside is not inspected.

## Node disk observations

Working-directory disk observations near the beginning and end of session turns are enabled by default. Set `STEVE_DISK_OBSERVE=off` in the Node process environment to stop new records. This does not change execution permissions, disk admission or cleanup. Samples are asynchronous observations with possible concurrent writers, not reliable baselines, attributable growth or permission to delete anything. They do not yet produce per-principal or per-project historical estimates.

Sampling budgets are configurable with `STEVE_DISK_OBSERVE_ENTRIES` (default `100000`), `STEVE_DISK_OBSERVE_DEPTH` (`64`) and `STEVE_DISK_OBSERVE_MS` (`500` milliseconds). Exhausted budgets preserve incomplete results. Each Node starts at most one sampler at a time. Disabling observations or shutting down cancels traversal at its next check; path validation and a single blocked system call cannot be interrupted. The async worker can stop waiting, but process exit may still wait for that blocking call to return.

Records stay in the Node journal. `STEVE_DISK_OBSERVE_ROWS` defaults to `1024`; `STEVE_DISK_OBSERVE_KEEP_MS` to `604800000` (seven days). Enqueuing new records removes expired or oldest completed statistics as needed. In-flight records are not evicted; a full queue increments a skipped counter. Values must be positive, representable integers. Invalid settings warn and disable observations without refusing Node startup. The Node host API offers paged readback; no Web or MCP endpoint is added.

Optional statistics failures are isolated from turn-state commits when SQLite can roll back the statistics savepoint. A failure that aborts the outer journal transaction still fails the operation; a successful run is never claimed after a lost business commit. Readback paginates current rows by insertion sequence, with a separate snapshot for each page. Pending records change in place and retained rows may expire between calls, so callers must restart from zero to refresh final states. It is not a completion event stream, and these records are not historical p90 inputs.

## Appearance and themes

Settings → Appearance chooses, per device, light or dark, an accent (Indigo, Blue, Violet, Green, Orange, Rose,
Graphite), the neutrals (Slate, Gray, Sand), the text font (Geist, System, PingFang first), the monospace font
(Geist Mono, System mono) and the corner radius (Compact, Standard, Rounded). The choice is kept in the browser's
`localStorage` under `steve-appearance` (light/dark under `steve-theme`); nothing is stored on the Hub, so the
"tell Steve" box on that page, which drafts Hub settings, does not change it.

Every choice is a theme manifest, and the built-in ones are written the same way (`web/src/themes/builtin.ts`). A
manifest overrides a whitelist of the design tokens in `web/src/index.css`, separately for light and dark:

```json
{
  "id": "plugin.ocean",
  "name": { "zh-CN": "海洋", "en": "Ocean" },
  "kind": "accent",
  "tokens": {
    "light": { "primary": "#0b5e8a", "primary-foreground": "#ffffff", "ring": "#0b5e8a", "blue": "#0b5e8a" },
    "dark": { "primary": "#0b5e8a", "primary-foreground": "#ffffff", "ring": "#2a86c4", "blue": "#5fb0e6" }
  }
}
```

- `id`: lowercase letters, digits, `.`, `_` and `-`, at most 64; a built-in id cannot be replaced.
- `name`: a string, or the name per language (`zh-CN`, `en`); the interface uses the one for its language.
- `kind` says where the theme appears and what it may set:
  - `accent`: `primary`, `primary-foreground`, `ring`, `blue` (links, selection, work in progress), `chart-1`, `sidebar-ring`.
  - `neutral`: `background`, `foreground`, `card`, `popover`, `secondary`, `muted`, `muted-foreground`, `accent` (the
    hover wash), `border`, `input`, `tertiary`, the `*-foreground` partners, `chart-2`…`chart-4`, `series-other`,
    the `sidebar*` tokens, `canvas-graph`, `code`, `raised`, and the shadows `lift`, `lift-hover`, `highlight`,
    `sh-xs`…`sh-xl`.
  - `sans` with `fonts.sans`, `mono` with `fonts.mono`: a font-family list of installed or bundled fonts (no URLs).
  - `radius` with `radius`: a scale from 0 to 2 on the corner radii (1 is Standard: 6, 8, 12, 16 px).
  - `full`: a skin, laid over the other choices; it may set every token above, the status colors (`destructive`,
    `attention`, `success`, `danger-icon`, `chart-5`, `series-1`…`series-5`), `fonts` and `radius`.
- Token names are written without `--`. A color is `#hex`, `rgb()`, `hsl()`, `oklch()`, `oklab()`, `lab()`, `lch()`,
  `transparent`, or `var(--token)` naming another whitelisted color token (Graphite uses `var(--foreground)` so it
  follows the neutral). A shadow is a `box-shadow` list of such colors.

Only the light text on `primary` and the `blue` text on the page and cards need WCAG AA (4.5:1); the built-ins are
tested for it on every neutral in both modes (`web/test/themes.test.mjs`).

Themes from elsewhere, such as a future theme plugin, are added in the web client with `registerTheme(manifest)` from
`web/src/themes` (also `window.steveThemes.registerTheme` for a script that is not bundled with the client). It
returns `{ ok: true }` or `{ ok: false, errors }`: a manifest with an unknown field or token, a token its kind may not
set, or a value outside the syntax above is refused as a whole and nothing on screen changes. A registered theme
appears among the choices of its kind; a stored choice whose theme is not registered shows the default until it is.
