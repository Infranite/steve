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
- [Conversation digest base](#conversation-digest-base)

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

Use only the commands for the role installed on that machine. Both roles also support `stop` and `restart`; `--home` and the original `--label` select a custom installation. Node connection and idle state must still be verified in Devices. Signing each harness in on each device is separate from installing Steve: Steve uses the harness's own sign-in on that device ([Harness setup](#harness-setup)).

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

A second installation is separate from the usual one in everything Steve keeps, but not in what the harnesses keep. The owner's main conversation, the first meeting and the model listing start each harness with the service's environment, so harnesses use the OS user's own state directory (`~/.claude`, `~/.codex`, `~/.gemini`, …): the same history, memory and settings you use yourself and the usual installation uses. An installation made to try something out, to test a build or to demonstrate Steve should not write there. Before its first start, give each of its harnesses a directory of its own in its `steve.env`, with the variable the harness reads its state directory from (`CLAUDE_CONFIG_DIR` for Claude Code, `CODEX_HOME` for Codex, `GEMINI_CLI_HOME` for Gemini CLI, or XDG variables for XDG-based CLIs; see [Harness setup](#harness-setup), or `state.env` in `harness.json` for a custom adapter):

```sh
mkdir -p ~/steve-trial-harness/claude ~/steve-trial-harness/codex
cat >> ~/steve-trial/steve.env <<EOF
CLAUDE_CONFIG_DIR='$HOME/steve-trial-harness/claude'
CODEX_HOME='$HOME/steve-trial-harness/codex'
EOF
steve hub start --home ~/steve-trial --label io.github.infranite.steve-trial.hub
```

`steve.env` holds literal values, so the paths are written out in full (the shell does that above). The directories start empty: the harness has no settings and no login there. Put the settings it needs into them (`settings.json` for Claude Code, `config.toml` for Codex) and sign in with the harness's own command and the same variable, as in `CODEX_HOME="$HOME/steve-trial-harness/codex" codex login`. Do not copy another installation's `auth.json` or refresh tokens. Sessions that run for a principal get their own directory under the installation's home either way ([`STEVE_PRINCIPAL_STATE`](#harness-setup)); every principal's session uses the sign-in of the directory configured here, and the owner's also starts from its settings.

Two things stay shared. The owner's main conversation works in the OS user's home directory with that user's access, so a harness still reads the instruction and configuration files it looks for in its working directory, and Steve itself can read and change whatever that user can: use a trial for what you would do at your own shell. And a real harness spends the same account's quota whichever directory it keeps its state in.

## The first meeting

The first time the instance's owner opens an empty Hub in the browser, a short tour asks them to choose a primary harness/model and optionally a separate lightweight harness/model before offering a first meeting. No provider is inferred as the owner's preference. The model choice is saved without inference, so this step works even when the previous provider is out of quota; the tour may also be skipped. Accepting it makes Steve open a conversation in its own main conversation; declining it, or skipping the tour, is remembered by the Hub for every browser. Other users only see the tour.

In the meeting Steve asks what to call you and what you use it for, and then, one at a time and only after you agree in the conversation:

- **Your profile.** It may read a small, recent part of what is already on the Hub's machine (the session history and global instruction files coding agents left in the home directory, the global git configuration, the projects under the home directory) and saves what it learned as your name, a short description and remembered facts and preferences under 我. It is told not to read or save secrets.
- **The models.** It lists the models each installed harness offers and picks, per purpose, a fast one for routing and titles and the strongest for research, drafting and orchestration. Only models a harness reports are accepted. Settings · Models shows and changes the result.
- **Bots.** From what you do and the Bots there are already, it proposes two to four Bots and creates the ones you agree to. The Bots page shows and changes them.

Every one of these writes goes through the same validation, audit log and setting revisions as the corresponding page, so each can be reviewed and undone there. Settings · Activity log lists them for the instance's owner, newest first: when the meeting began and how it ended, what each step saved (the profile's fields and the number of remembered facts, the purposes set, the Bots created), and under them the single writes, each with who did it and, opened, what changed. The meeting runs as the owner's main conversation does, with the access of the OS user the Hub runs as, and it uses the model of Steve's own Bot, so it spends that harness's quota.

The meeting ends when Steve finishes it, when you say you want to skip it or press **Skip introductions** under its welcome, or by itself after twelve of your messages. Until then, what you say in the main conversation goes straight to Steve without the routing step, except a message that names a Bot or an item. After it, changes go through the usual pages and change cards; its tools no longer exist. Settings · General · Onboarding starts the tour, and with it a new meeting, again.

The model shortcut is also `POST /api/settings/models/select` with the owner’s access token: `{harness, model, expected_revision, follow_purposes, lightweight}`. Read `bot_revision` from `GET /api/settings/models` for `expected_revision`; `model: null` explicitly selects the harness default, and `follow_purposes: true` clears the current workspace’s purpose overrides. `lightweight` is `{harness, models: [id], effort?: string}` or null to follow the primary model; it is shared by all four lightweight purposes. A stale revision returns 409 without writing. The endpoint does not make a model request.

A Hub driven without the web client can use `POST /api/onboard/start` and `POST /api/onboard/skip` (the owner's access token) for the same two actions.

### The activity log

Settings · Activity log (the instance's owner only) shows the workspace's audit trail: who changed what and when, newest first, fifty at a time. An entry opens to what its writer recorded: for a changed setting the old and the new value, for a created Bot its name. Ids are shown by what they are called now. The same list is `GET /api/audit?limit=<1..200>&before=<entry id>` with the owner's access token: `{entries: [{id, ts, actor, action, target, detail}], names: {<id>: <name>}, more}`; pass the last entry's `id` as `before` for the next page. The Hub does not change or remove entries.

### Models without the meeting

Steve has two model roles. The primary model handles conversation answers, research, memory extraction, YOLO answers, drafting, mail and channel replies. An optional workspace-wide lightweight model handles routing (including channel-rule intent), titles, disclosure checks and flow Bot judgments. Without a lightweight choice, these follow the primary model. Each role can use a different harness; only explicitly chosen providers are used. The actual model and source for all eleven purposes are shown under Settings · Models. Individual purpose overrides have priority and are kept under Advanced, with a visible warning when they differ from the role defaults.

Discovery can suggest a quick model, but no longer creates the four lightweight overrides automatically. Explicit operator environment lists (`STEVE_MODELS_FAST` and `STEVE_MODELS_CHECK`) still initialize older settings once; existing per-purpose choices are preserved until the owner replaces them through “Configure both models” or edits them under Advanced.

The main conversation also has a model control above its composer, and failed answers offer a model change followed by an explicit retry. The choice changes Steve's default across workspaces and this workspace's new-session default on that harness. By default, it clears this workspace's per-purpose overrides (including the initial fast-model choices), so routing does not continue to use an exhausted provider after the switch. Clear the checkbox to keep those overrides; the panel names the affected purposes. Existing Bots and running turns keep their configuration. These writes are atomic, audited and revision-checked; a stale panel must reload before saving. Only the instance owner can use this shortcut. Installed/discovered models are not proof of sign-in or remaining quota, and Steve never moves to another provider without the user's choice. Retry resends the original message and attachments; completed operations can run again. Native-only adapters without a read-only mode are not eligible for Steve's judgments, but remain available for agent sessions with their declared permissions.

The role allocation is one atomic save. Clearing an individual override returns its purpose to the relevant role, so a later role change applies automatically. Disabling the lightweight role returns its four purposes to the primary model, while any explicitly preserved overrides continue to win.

### Bot model validation

Bot creation, model/effort edits and configuration-card applies use the selected harness's last successful model and reasoning-effort report. Use its exact model ID, including any provider or route prefix; a display name or conversational abbreviation is not an ID. Reasoning efforts come from the report too, so a newly offered value does not require a Steve release. A successful empty report offers no explicit choices: reset the field to default instead.

For `POST /api/bots` and `PATCH /api/bots/<id>`, `model: null` selects the workspace's agent default for that harness, or the harness's own default when no workspace default is set; `reasoning_effort: null` leaves effort to the harness. An omitted patch field keeps its stored value. A harness change checks the model and effort it would keep, and rejects an incompatible selection before saving any part of the patch. Supply supported values or explicit `null`s in that same patch; Steve does not silently rewrite them. An unrelated edit (such as `role` or `rules`) leaves old selections alone, and repairing just one selection field does not rewrite another.

The catalogue is a cached Node probe, not a model-call health check. Before a successful report, or when the adapter cannot report its choices, support remains unknown: model IDs must still satisfy Steve's ID syntax, and effort must be nonempty text of at most 100 bytes. Saving in this state does not certify availability. A failed Node refresh retains the previous report; a failed adapter probe does not prove that a model was removed. Check **Devices → runtimes** and refresh the model list to inspect current evidence. On session open, the ACP layer still checks whether the requested model took effect and whether that session offers and applies the effort. That session check can fail after a valid save, for example after an adapter update or an authentication failure. The catalogue does not describe effort support for every individual model.

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
- `daily-YYYYMMDD-files.tar` holds what the database alone cannot bring back, as far as it is in the home: attachments (`data/files`), item deliverables (`workspaces` and `deliveries`), the signing identity (`signing`, without a keychain password file), the secret files (`secrets`) and `steve.env`.

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

**Devices → Clients** (`#/devices/clients`) shows this client, the other clients signed in to the Hub and the access tokens. A client not seen for 7 days is listed under "not used lately", and one (or a token) unused for 30 days is flagged. Signing a client out can revoke the token it signed in with too. The instance owner sees every member's clients there and can sign any of them out (`GET /api/access?all=1` lists them with the members' names under `users`; `DELETE /api/access/clients/{id}` accepts anyone's for the owner); anyone else sees and signs out only their own.

The Hub records each client's kind and name, and the route it last came by. Routes are `loopback`, `lan`, `tailnet`, `proxy` or `public`, with `+tls` added when the request arrived over TLS. Forwarding headers are believed only from this machine or the local network. Failed sign-ins from another address are slowed down after ten tries in ten minutes.

`GET /api/access` lists the caller's tokens and clients. Each client carries `token_id` (the token it signed in with, `null` for a paired one), and whether it is online: `online` is `true` while it holds the live event stream (`GET /api/stream`) open, `streams` counts its open streams and `connected_since` says since when it has had one (Unix milliseconds); presence is kept in memory, so after a Hub restart a client is offline until its page connects again. The stream tells the client's user, and the Hub's owner, when a client comes onto it, leaves it, or is signed in or out: `{"type": "client", "id": "<client id>", "user": "<user id>"}`. Signing a client out (`DELETE /api/access/clients/{id}`, its own sign-out, revoking the token it signed in with) ends its open stream at once, and its reconnect is refused. A client revoked with `steve-hub --access` while the Hub runs loses its stream within 30 seconds.

Manage tokens and clients on the Hub's machine, also when it does not answer. `steve-hub` is the installation's own binary, `<home>/current/bin/steve-hub` (`~/.steve/current/bin/steve-hub` for the usual installation); it is not on the `PATH`. For an installation in another directory, name it with `STEVE_HOME=<home>` in front of the command, as the line `steve setup` prints when it is done does.

```sh
steve-hub --access token create [--user NAME] [--name LABEL] [--days N]   # shown once
steve-hub --access token list
steve-hub --access token revoke ID        # signs out the clients that used it
steve-hub --access client list
steve-hub --access client revoke ID
```

On a Hub nobody has signed in to yet, `token create` makes `--user` (default: your account name) its owner.

Single sign-on through OIDC is optional. The owner sets it up in **Devices → Hub → Signing in** with an issuer, a client ID and a secret. It takes effect without a restart. The secret is kept in `secrets/oidc-client-secret` (mode 0600). The page shows the callback to register with the provider. An `STEVE_OIDC_*` environment configuration takes precedence and is shown read-only. When the web client has a port of its own, its `/auth/…` paths redirect to the Hub's port, so a callback registered on the web address works.

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

- **Where.** The Hub switcher is at the top of the sidebar (on a phone: the **Me** page). It lists the saved Hubs, then **Devices & connections** (the devices page of the Hub in use), **Add device…** (a Node, or a phone or browser by QR code) and **Add or manage Hubs…**, the full list where a Hub can be added or forgotten. The sign-in screen and the "cannot connect" screen show the same list, and <kbd>⌘</kbd> <kbd>K</kbd> has the same entries. A link can open them directly: `#/hub/add`, `#/hub/pair`. In a browser, a pairing opened by a link makes its code only after a click.
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
- **macOS native shell.** The toolbar shows the current Hub, page title and the workspace actually selected in the web client (or explicitly says it is not yet confirmed). Its Hub menu supports switching, adding/managing Hubs and pairing devices. Connection details show the endpoint, connection route, Hub version and reconnect action. The menu-bar item offers the same context and useful navigation rather than session/device counters. The business sidebar, workspace switcher and page actions remain in the web client; only the duplicate global Hub switcher is hidden when native bridge v2 explicitly advertises `shell.chrome.hub`. Browser, older-native and iOS clients retain their web Hub navigation.
- **One macOS main window.** Open Steve, menu navigation, Dock reopen and pairing links restore or unminimize the same main window, creating it if closed. Closing it leaves the menu-bar client running. **Quit Steve** / <kbd>⌘</kbd> <kbd>Q</kbd> quits only the client, never the Hub. Native **Settings…** and the **Navigate** menu are available; web composer shortcuts remain web-owned. A `steve://pair` link still requires the native pairing confirmation before claiming it.
- **Attention foundation, not reminder delivery.** Attention separates Steve replies, pending work, completions/deliveries and connected-channel messages, with links to their source pages. All four personal reminder categories explicitly report unavailable in this version: no synthetic unread counts, recent messages or delivery claims. The app does not request notification permission or deliver background reminders; background connection checks are not reminder monitoring.
- **Shell bridge contract.** Additive bridge v2 operations are `shell.context`, `shell.navigation.next` (a direct intent or `null`) and `shell.navigation.ack`. Context contains only selected-workspace/page metadata, readiness, authentication, focus and visibility; native assigns Hub/document/generation identity after checking the current main frame and WebView owner. Native navigation is scoped to the Hub and optional workspace, uses the existing workspace-switch flow and acknowledges only the matching issued intent after readiness and its canonical destination. Hub add/pair actions require the actual dialog; settings aliases resolve to Devices. Failures are visible and stop automatic retries. Deep `targetID` navigation is not supported yet and fails explicitly, rather than claiming that a reply was opened. The native Hub chrome capability defaults off and is enabled only by the macOS shell.
- **Building from source** (macOS 14+ with Xcode):

```sh
./scripts/build-apple-client.sh
```

Individual targets can also be run directly (`./scripts/build-apple-client.sh test`, `mac`, `ios`, or `ios-sim`).

## An external channel service

A messaging system Steve has no built-in adapter for can be connected through a separate channel service: a program that signs in to that system as you or as a bot and speaks Steve's external channel protocol over a WebSocket. The service runs on its own (on your machine or LAN); the Hub dials out to it, so the Hub opens no port for it, and a crash or restart of the service only pauses the channel.

- **Connect:** in **连接 · 其他聊天工具** (Connections · Other chat app), enter the service's address (`wss://…`; plain `ws://` only to `127.0.0.1` or `::1`), the access token the service issued for this Hub, and, for a self-signed certificate, its pin (`sha256:<64 hex digits>`). The Hub checks the address, says hello, asks the service who is signed in, and the connection becomes yours.
- **Receiving** follows the connection's `msg.receive` capability. Messages written before the connection was turned on, read back after an interruption, or your own, are only recorded.
- **Sending as you** (replies, reactions, recalls) goes through the service, which may refuse or limit it; a service that forbids writes leaves the connection read-only.
- **The protocol** a service implements, with its conformance checklist and test vectors, is the documentation of `steve_channels::adapters::ext_ws::protocol` (`hub-rs/crates/steve-channels/src/adapters/ext_ws/protocol.rs`). The Hub's test suite runs a fake service against it (`hub-rs/crates/steve-channels/tests/ext_ws.rs`).

### Bot and self identity

Select the identity **before connecting**: `POST /api/connections` uses `adapter: "ext_ws"` for self or `adapter: "ext_ws.bot"` for a bot, with the same URL/token/pin credential fields. The Connections page exposes both providers. A channel/connector plugin selects the same provider with an index entry's `identity: "self" | "bot"`; omitted identity means self. An identity is per connection, not a flag that converts an existing account. A known human account cannot be adopted by the bot provider, nor a known bot account by the self provider; disconnecting does not turn one into the other.

For a bot, both the `hello` result and `me.get` result **must** contain `"identity": "bot"`. For self they may say `"identity": "self"` or omit it, retaining the original protocol behavior. Unknown/null values, disagreement between hello and account, or a declaration that differs from the selected provider refuse the connection before any message is sent. `me.get.user` keeps the same primary handle, display and aliases for either identity. The verified kind, realm and primary handle are remembered and checked on subsequent sessions; identity changes require reauthorization, never silently switching to send-as-you.

- Self uses `Mode::Own`, `msg.receive` and `msg.send_as_user`. Its proven handles may be claimed into “me”; own-message suppression, reply approval, reactions and recall keep their existing behavior.
- Bot uses `Mode::Bot`, `msg.receive` and `msg.send`, `accounts.kind: "app_bot"` and a bot contact, not the connecting user's “me”. Its aliases are attached only under the ordinary account-ownership rules; no human account is reassigned. Bot replies go through the normal authorized outbox and a bot channel adapter, not `ActAsUser`. This minimal bot adapter supports text send/thread reply and stable `source_id` receipts, including replies and echoes without platform message IDs. It does not advertise reactions, recall, editing or member listing, nor promise that it receives every platform message.
- Every write carries `expected_identity: "self" | "bot"` alongside `expected_sender` and the existing idempotency key. A bot-aware service **must** check both identity kind and handle against its current credential **before any side effect** and return `account_changed` on either mismatch, including a switch under the same handle on a live socket. It must preserve the same stable `source_id` in the send result and feed echo, and retain send idempotency keys for at least 24 hours.

These are generic external-service contracts only. No built-in platform adapter is removed or migrated, and notification defaults and existing platform adoption workflows are unchanged.

## Workflow execution

`STEVE_WORKFLOW_MAX_PARALLEL` limits claimed collaboration units **per root item tree** (default `4`; unset, invalid or nonpositive values use the default), not across the Hub. All work graphs belonging to a root item and its recursive provenance descendants share that budget, including graphs on ordinary forked items and trees whose root has no graph. Unrelated root items have independent budgets, so their combined collaboration concurrency can exceed four. An explicit graph `max_parallel` can further restrict that graph but cannot reset or enlarge the shared tree budget. Manual sessions and coordinator sessions are not additional collaboration units. Workflow units still cannot create nested work graphs.

Node capacity is separate: every execution on the same Node shares its configured `max_sessions` ceiling (default `32`), including work from different item trees and ordinary sessions. Ready work queues when either applicable budget is full; completed or stopped work frees its reservation only after execution has settled. A paused graph with confirmed stopped execution no longer occupies the tree budget. Changing an item's provenance moves its graph's accounting to its current root; existing execution is not cancelled if the destination tree is already full, but no further units are admitted until space is available. Broken, cyclic or cross-workspace parent chains block new scheduling for the affected graph rather than granting a fresh budget.

This setting previously imposed a single Hub-wide collaboration limit. Operators upgrading with several active root items should expect more aggregate execution, bounded by each Node's capacity. Set Node capacity according to the machine's actual resources; lowering the tree budget restricts each tree independently and is not a replacement for a Node-wide ceiling.

A multi-Bot work graph coordinates ordinary item sessions. A child inherits the parent's `requested_by`, workspace, project and routing tags; its Bot remains the one assigned by the graph. Execution is admitted only when the parent and current child have the same confirmed request principal. A current workspace member is a user principal; an item with no requester explicitly uses the workspace service principal. An unknown or departed requester is blocked for workflow execution rather than silently changed to the service principal. Existing live Node bindings must agree with that identity. Cross-principal collaboration is refused.

Children use the ordinary Node binding and execution-policy path, including Bot membership and enabled state, roots, permission modes, Node ceiling, approvals, principal harness state and environment settings. Workflow itself does not require the `os-sandbox` or `principal-isolation` capabilities and does not claim to provide multi-principal OS isolation. Confinement remains the Node's independently configured policy. The current remote Node single-owner workspace restriction still applies; remote session placement still accepts only project-free work in scratch workspaces.

Git projects must be active in the same workspace with item worktrees enabled. Each child uses a separate worktree; work without a project uses a separate item directory. A stopped, disconnected or unauthorized Node, an unmet routing tag, a changed principal or an unavailable Bot prevents new execution. Work never falls back to a Hub harness. The item page reports admission failures under collaboration blockers; fix the stated configuration or identity problem, then resume a paused graph or retry a failed unit within its attempt budget.

The saved graph, attempts, child items, sessions and Node turn ledger survive restarts. Recovery reuses an inserted session even if its first Node binding was interrupted, and reconciles an already running turn before continuing. Repeated scheduling does not create a second child or replay a claimed turn. A retry creates a new attempt and retires the old one; an old session cannot gain access by detaching from the graph. A failed work unit closes its child item as failed after the turn ends, and a cancelled one closes it as cancelled; recovery also closes older current or retired attempts once their turns are no longer running. A retired failed delivery keeps its failed outcome and reason, also when that attempt's turn end arrives after the retry, while a retired attempt without failed delivery is cancelled. The result remains in the graph history for the coordinator to replan or retry. A unit out of attempts holds every unit after it, and the coordinator is told instead of the graph waiting as queued. A pause marks the exact turn it stopped, in the database: only that turn's end counts as the pause, also when the graph was resumed and the Hub restarted before that end arrived, so a later turn you stop yourself still fails its unit. Each turn end is recorded per attempt, so the same end delivered again, also after a restart, is not judged twice and does not undo your Stop; a Stop is kept for the turn it named, and the late end of an older turn does not use it up. A stop alert left open before a restart expires when the current graph shows work running or a definite wait; actual user questions and uncertain external effects remain open. Pauses and prerequisite waits hold queued follow-ups too. Dependent work waits for successful delivery, the upstream turn to end and its item to finish closing. Independent review and integration acceptance remain required before the parent can finish; workflow delivery does not authorize a release or deployment.

The items list reads the work graph's pause flag, cycle and failed units. A parent with a paused graph in its current cycle is grouped under **Paused**, even when the parent item's lifecycle status is still `run`; its row names failed current-unit child attempts. That applies to a graph you paused. A graph its coordinator Bot paused without asking you anything waits for you instead: Steve opens a card with the coordinator's pause reason (when its turn ends, or within a few minutes for a graph paused earlier), the item moves to **Waiting for you**, and Steve does not continue the coordinator over its own pause. Resuming the graph withdraws the card. A pause recorded before this release counts as the coordinator's unless it came from pausing the item itself. An active parent lists only unfinished workflow children as current waits. A work graph from an earlier cycle is labeled as a prior-cycle record, not as the current cycle's pause. Historical closed children remain available in the graph and item history without being shown as current waits.

The item page derives one state for the item — waiting for you, a work graph to replan, unavailable or paused collaboration, working, queued, idle or closed — and every place on the page (header, "now" card, the collaboration header) shows that state. An item that coordinates a work graph or started other items shows its collaboration under the "now" card as one row per step, in dependency order: the step's Bot and model, what it is doing or how it ended, on which try it passed, a progress bar, and its working time and cost with earlier tries included. A step's earlier attempts fold into its row and are listed in its detail. Steps that need you, failed or are running always show; of the finished ones only the latest three, the rest on request. The header counts what needs you, failed, runs and is done (with retry for failures), and shows the coordinator, the elapsed time against the agents' working time and the estimated cost of the whole subtree from `GET /api/stats/tree/{id}`, which returns the item and the items started from it (at most eight levels and 200 items) with each one's own usage and agent time. "查看全图" opens the steps as one dependency graph, where an arrow means the later step waits for the earlier one and steps that could run at the same time share a column. Selecting a step opens its detail beside the page, or as a sheet on narrow screens; open the linked child item or session for its full record. The items page's 协作 view lists every open coordinating item with its steps. A running unit can be stopped individually. Reworking a completed unit previews its downstream invalidation, requires active work to stop and settle, and uses the existing attempt budget and revision checks before resetting; earlier child items remain available in the archive. A running workflow unit is labeled confirmed execution only when the Hub has applied a recent assistant, tool, permission or plan event for the exact active session, turn, attempt and Node, after that attempt started. The Hub receive time must be less than ten minutes old, and the Node must still be active and recently seen. Missing, stale or uncertain evidence is shown as active but unconfirmed; it does not change scheduling or approval authority. The relation graph, opened from the item menu, can show blocking dependencies or provenance, with missing references visible and ownership edges distinct from dependencies. On narrow screens the same relations appear as a structured list; selecting a node shows its detail before an explicit open action. Archived attempts are read-only links to their child items. They do not count as current-cycle delivery.

The work graph API shows bounded unit summaries and the latest 20 plan changes; full audit history remains in storage, and full non-Git deliverables are available from the child item's outputs. A provider usage-policy refusal or invalid API request has no review verdict: Steve records the failed attempt, keeps successful upstream units, and requires a user to retry after resolving the provider policy or request configuration. A graph from a prior item cycle does not grant acceptance for the new cycle. After the old graph is paused and every old execution has stopped, replanning archives its units with their original verdicts and starts pending units for the current cycle. Old successes remain visible as history but never satisfy the new cycle's review or integration gates.

For a design or report delivery, Steve retains the delivery text as an immutable Markdown delivery on the Hub and registers its version URL as an item output. A design-only goal, or one that explicitly defers implementation until design confirmation, opens a user-only decision bound to the exact delivered bytes and goal version; choosing implementation continues the same item, while choosing design only closes that design cycle. Earlier path-based design deliveries remain valid and retain their content checks. The design file is the output of kind 方案, 设计 or 报告; failing that, any output (not a `record`) whose location is the exact URL of a file this item received with `steve_deliver_file`, when it opens in the browser: HTML, Markdown or text, PDF, an image, or a ZIP with an entry.

### Review findings and merging

A work graph splits by independent scope, not by phase: planning and implementing one scope stay in one unit, and review goes to the independent review Bot. A review unit's findings are blocking or non-blocking by a fixed category table, not by the reviewer's judgment of severity:

| Blocking | Non-blocking |
| --- | --- |
| correctness, security, data loss, the goal not met, a required check failed | style, naming, documentation, optional optimisation |

A blocking finding keeps the work from merging. A non-blocking one is recorded on a carry-over list, and the delivery must answer each entry: fixed, or why not. This is the target contract and is being implemented; until then a review verdict is pass or fail with a free-text reason.

A git item that changed code without a work graph had no independent review. It is not merged automatically: once its delivery is accepted, Steve shows a card for its pull request, a person merges it, and YOLO does not answer that card. This is being implemented together with the pull-request auto-merge fix.

### Inline reply images

Completed assistant Markdown images in ordinary Node sessions and research jobs are captured on the execution Node and kept in the Hub's existing attachment store (`<home>/data/files`). Successful references keep their alt text and use `/api/files/<id>?ws=<workspace>`; both sign-in and workspace access are required. Source deletion, execution cleanup and Node disconnection do not remove a retained image. This is separate from file delivery and public sharing, and adds no browser endpoint for reading a machine path.

The conversation client loads Hub image references through its currently connected Hub, with credentials and the workspace specified by the image URL, even if another tab selects a different workspace. Failed loads show a localized explanation and a retry button. A Node's local path instead asks for regeneration or upload. Relative images inside a file-delivery package still resolve within that package.

Capture reads only the execution directory and explicitly granted readable roots, with no traversal, symbolic or hard links, special files, hidden/sensitive paths or control-plane access through a broader root. Full-access research sets `TMPDIR` to its own attempt's `scratch/tmp` so tools can create temporary QR codes there. A tool that hardcodes a shared `/tmp` path must instead write into the execution directory or honor `TMPDIR`; Steve does not scan global temporary directories or recover historical deleted files. Restricted runs gain no extra readable directories. GFM image ranges, destinations and titles must agree with the semantic-alt parser before any local read. External URLs, GFM literal autolinks (including `https://host/![alt](path)` and `www.host/![alt](path)`), ordinary links and code samples do not cause local reads. Unconfirmed syntax stays unchanged.

Each attempt captures at most ten images, each at most 20 MiB; PNG, JPEG, GIF and WebP content is decoded with an 8,192-pixel dimension limit and a 64 MiB allocation limit; animations are limited to 100 frames and 64 MiB of decoded frame data. The Node holds the message suffix starting at a possible Markdown image until a message boundary, so incomplete code examples cannot trigger reads and stored history contains the final reference. Other text keeps streaming. Capture has a five-second total wait budget per attempt. A failed capture preserves the original Markdown and does not fail the text reply.

The existing `blob.begin/chunk/status/finish/abort` Node-to-Hub contract checks the live execution, workspace, chunk offset and hash, then the full size, SHA-256 and image content before publishing an attachment. `Transfer.workspace` is optional for older peers and always assigned by the Hub. Unsupported older Hubs leave replies unchanged. Partial uploads use 16 KiB chunks and bounded Hub memory (40 MiB per Node, four open transfers) with a ten-minute crash fallback reclaimed on a subsequent begin. Complete content with an invalid total hash, decoded image or container is immediately aborted and its partial bytes and open slot are released; aborted metadata is bounded and does not consume active quotas. An incomplete finish or a rejected chunk can still resume with the correct suffix/chunk. The Node aborts on final failure and requests bounded cleanup when capture times out or is cancelled, including a lost begin reply. Cleanup of the authenticated Node’s own transfer remains allowed after execution cancellation; any later begin also reclaims open uploads of ineligible executions before applying quotas. If the Hub is unreachable, immediate cleanup cannot be acknowledged and the next-begin/expiry fallback applies. Retry resumes offsets, including across reconnects to the same Hub process, or starts over after a Hub restart. Completed bytes are deduplicated by workspace and content, and atomically written with a synced temporary file and rename. An accepted attachment whose message is later lost is retained like existing attachments; there is no automatic attachment garbage collection. Database insertion failure can leave a complete content-addressed blob, reusable by retry, but never a public reference to partial bytes. Restore the attachment files and database together from the same backup.

### File delivery and independent sharing

A bound Node resolves file delivery paths (absolute or relative) inside the workspace captured for the current turn. The Hub's session directory may be a placeholder on another machine and is not a path mapping. Both `steve_deliver_file.path` and actual files in `steve_deliver.outputs.location` use this boundary; HTTP(S) references retain their link semantics. Upgrade older Nodes if execution-scoped delivery is unsupported, then start a new turn to capture its boundary. Traversal, symlinks, hard links, replaced workspaces and stale executions are refused.

A Bot calls `steve_deliver_file {path, title?, entrypoint?, interactive?}` in any session whose working directory is known: item sessions, chats and conversations, and scheduled runs. `path` is an actual file on that session's Node, inside its working directory, or, in an item session, a file already placed in the item's Hub-owned `<home>/workspaces/<item>/outputs`. Package a folder as a ZIP first. A delivery made outside an item has no item; it is managed by the member the session works for, or else by the workspace's owner, and appears in that person's **我的发布** list. The Hub reads a bound session's source through the Node's strict `delivery.execution_read` face (`delivery.read` for unbound legacy directories), pins its revision across chunks and requires exact offsets, total size and EOF before accepting it. With the local Node off, the same descriptor-based reader handles the local session directory. Older Nodes without the required strict delivery face fail explicitly; upgrade the Node instead of falling back to an unsafe filesystem read. The original file limit is 64 MiB and transfer time is bounded to five minutes. Symbolic links, multiply-linked files, special files and paths outside the allowed directory are refused.

The tool returns JSON containing `id, item_id, workspace_id, title, name, mime, size, created, entrypoint, interactive, scripts, reachability, url, download_url, content_url, frame_url, access_warning` (`item_id` is null outside an item). Times are Unix milliseconds. `url` is the authenticated `/deliveries/:id?ws=<workspace>` page address, built from `STEVE_PUBLIC_URL`; set that to the browser-reachable installation address before delivering. Preserve its query string. The page client must bind `ws` before requesting `/api/state` or metadata, so a different remembered workspace cannot hide a permitted delivery. There is no machine hostname or source path in the result. Receiving does not end the turn. Reference the returned `url` in a `steve_deliver` output to associate the exact version with the item's delivery/card. `steve_deliver` also receives ordinary file outputs automatically; branch, commit and external link outputs keep their existing behavior. A new receive always produces a new version ID, even for a repeated filename. It never changes an earlier URL's bytes.

The item detail API's `artifacts` entries include nullable `delivery_id`, shared by item outputs and chat delivery cards. It is present only when the artifact's full location exactly matches a URL registered in this Hub's delivery store, for the same item and its current workspace. Consumers should use that field for delivery previews and downloads, never extract an ID from arbitrary external URLs. Old registered full URLs also receive the association; older paths, unregistered URLs and links to another item's delivery keep their existing behavior with a null `delivery_id`. Coordinators may still include upstream delivery links within the same workspace, without reassigning the upstream version's item.

`access_warning` identifies configured loopback/localhost and wildcard bind addresses as unusable from another device. For other addresses it says that reachability has not been verified; a hostname or LAN address alone is not evidence of external access. HTML/SVG and HTML packages additionally say whether the preview is static or interactive in the same field. `reachability` is the same check as a value (`local` or `unverified`); the delivery page shows it in the reader's language instead of the English `access_warning`. Check it before telling a recipient to use another device; setting a public URL does not test DNS, routing or firewall access.

The Hub keeps content-addressed objects in `<home>/deliveries/objects`, separate from Node workspaces and worktree cleanup, and commits their database descriptors only after all objects are synced. Node disconnection and session completion do not affect an accepted delivery. Restore the database, `deliveries` and legacy `workspaces` together from the same daily backup. No automatic object garbage collection is performed; an interrupted receive can leave unreferenced objects. Deleting a source file or revoking a share does not delete the retained delivery.

For ZIP packages, `entrypoint` selects the file the package opens to: any existing package file (an HTML page, a Markdown document, a PDF, an image…). If omitted, the Hub selects root `index.html`, otherwise the first HTML path in lexical order, otherwise root `README.md` or `index.md`, otherwise the first Markdown path; a ZIP with none of these remains downloadable with a null entrypoint. Relative CSS, images and page links stay inside the package's resource URL tree. Validation rejects traversal, absolute and Windows paths, links and special files, duplicate or colliding paths, encrypted/unreadable entries, CRC/read failures and incomplete archives. ZIP64 and split archives are refused before central-directory allocation. Limits are 1,024 entries, 32 MiB per entry, 128 MiB expanded total, and at most `200 × compressed size + 1 MiB` per entry. Names must be UTF-8 plain paths with no empty or dot components, control characters, backslashes, colon, `%`, `?` or `#`; use spaces and ordinary Unicode names as needed. Nested archives are retained as bytes and never recursively extracted.

HTML and SVG content is served from `/content/…` with a response-level static sandbox, including on direct navigation and subsequent package pages: scripts, forms, workers, frames, plugins and network connections are disabled. CSS, images, fonts and media can load only from the same origin (and image/font data URLs). This is the default for every delivery, and `/content/…` stays static whatever the delivery's switch says. Do not remove the server CSP in a reverse proxy. When the previewed HTML contains a script element or an inline event handler, metadata reports `scripts: true`, and the page says so instead of showing an empty frame.

**Interactive HTML preview (opt in per delivery).** A delivery's `interactive` switch is off by default. The publisher may turn it on when publishing (`steve publish --interactive`, `steve_deliver_file {interactive: true}`, `?interactive=1` on the upload), and a user who manages the delivery may switch it at any time on its page or with `PATCH /api/file-deliveries/:id {interactive}`. Who manages a delivery: its owner (the uploader, or the member a session outside an item works for); for an item's delivery, the member who asked for the item or an owner of the workspace. Public shares follow the same switch, checked on every request. While it is on, the page shows the package entry in `<iframe sandbox="allow-scripts">` — without `allow-same-origin`, `allow-forms`, `allow-popups` or `allow-top-navigation` — loaded from a frame URL that is itself the authorization:

- `frame_url` in signed-in metadata is `/api/delivery-frames/<grant>/<entry>`: a random grant for that user and delivery, kept in the Hub's memory for 12 hours (a restart or reload issues a new one). Every request rechecks the grant, the user's current workspace membership and the switch. No cookie is read on this path. A share's `frame_url` is `/api/delivery-shares/<token>/frame/<entry>`, checked against the live share.
- Frame responses carry `sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' <frame prefix>; style-src 'unsafe-inline' <frame prefix>; img-src/font-src/media-src <frame prefix> data: (blob: for images and media); connect-src 'none'; form-action 'none'; frame-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'self' <STEVE_PUBLIC_URL, STEVE_PUBLIC_URLS and STEVE_CLIENT_ORIGINS origins>`. The document always has an opaque origin, also if loaded directly: it cannot read Steve's cookies, local storage or other pages, and cannot call `/api`, `fetch`, XHR, WebSocket, EventSource or beacons. Inline scripts and styles and the package's own files work; external CDNs do not, so bundle every asset.
- A top-level load of a frame URL (`Sec-Fetch-Dest: document`) is refused, and the delivery and share pages are served with `Content-Security-Policy: frame-src 'self' <this installation's public origins>`, so a script cannot navigate its frame to another site.
- What remains possible inside the frame: reading the package itself (which its viewer already has), posting messages to the page (which ignores them), and channels no CSP covers, such as WebRTC in some browsers. Turn the switch on only for packages you are willing to run as a web page.

Other documents use the existing preview shapes, exposed as the optional `preview` field of REST metadata. The page previews Markdown (rendered without raw HTML; relative images and links resolve to files of the same package and never above it), PDF inline, images, audio and video players (byte ranges supported), Office documents, and text and code with syntax highlighting chosen by extension. Text responses are labelled `charset=utf-8`; `.md` is stored as `text/markdown`.

**A user's own publications.** `POST /api/file-deliveries?name=<file>[&title=][&entrypoint=][&interactive=1]` with the raw bytes (or multipart field `file`) publishes a file without an item, with the same 64 MiB limit, validation, object store and immutable versions. It requires a signed-in user (a browser session or a user access client), the explicit `x-steve-ws` header and workspace membership; Bot session, MCP and Node tokens cannot use it. `GET /api/file-deliveries` lists the deliveries the user manages as owner in that workspace, newest first; the web client shows it as **我的发布** (`#/publications`, in **More** and the command menu), with each delivery's page and its shares.

From a terminal: `steve login --hub https://… --user NAME` signs this machine in as a user access client (the access token comes from `STEVE_ACCESS_TOKEN` or a hidden prompt, and only the resulting client credential is kept, in `$XDG_CONFIG_HOME/steve/cli.json` or `~/.config/steve/cli.json`, mode 0600). Then `steve publish <file|folder> [--title T] [--entry PATH] [--interactive] [--workspace ID] [--json]` packs a folder as a ZIP (regular files only; links and special files are refused, `.git` and `.DS_Store` are skipped), uploads it and prints the delivery page URL. `STEVE_CLIENT_CREDENTIAL` with `STEVE_HUB_URL` replaces the saved login.

The file-delivery REST contract uses `/api/file-deliveries`; `/api/deliveries` remains the separate channel-message delivery ledger. The user-facing page stays at `/deliveries/:id`.

| Method and path | Result |
| --- | --- |
| `GET /api/file-deliveries` | The signed-in user's own deliveries in the workspace, newest first (metadata objects). |
| `POST /api/file-deliveries` | A user's upload (see above); returns metadata. |
| `GET /api/file-deliveries/:id` | Authenticated metadata: `id,item_id,workspace_id,title,name,mime,size,created,entrypoint,interactive,scripts,reachability,can_manage,url,download_url,content_url,frame_url,access_warning,preview`. `item_id`, `entrypoint`, `frame_url` and `access_warning` may be null. |
| `PATCH /api/file-deliveries/:id` | `{interactive: bool}` by a user who manages it, with an explicit `x-steve-ws` header; returns metadata. |
| `GET /api/file-deliveries/:id/raw` | Original bytes as an attachment; single byte ranges supported. |
| `GET /api/file-deliveries/:id/content/*path` | Inline original document (its filename), or a validated package member, with sandbox CSP. |
| `POST /api/file-deliveries/:id/shares` | Independent user action with `{expires_at: Unix milliseconds}` strictly in the future; returns `{token,url,expires_at}`. |
| `GET /api/file-deliveries/:id/shares` | Array of `{token,url,expires_at,created,revoked_at}`; includes revoked and expired grants for management. |
| `DELETE /api/file-deliveries/:id/shares/:token` | Revoke immediately; returns `{ok:true}`. Repeating a revocation is safe. |
| `GET /api/delivery-shares/:token` | Anonymous metadata using share-specific `/shares/:token`, download and content URLs; no workspace identifier. |
| `GET /api/delivery-shares/:token/raw` and `/content/*path` | Anonymous original or resources, checked against the live grant on every request. |
| `GET /api/delivery-shares/:token/frame/*path` | The interactive frame of a shared delivery, while the share is live and the delivery interactive. |
| `GET /api/delivery-frames/:grant/*path` | A signed-in page's interactive frame, authorized by its grant only. |

Login and workspace membership follow the normal API authentication. A delivery URL is not public authorization. Explicit workspace headers/query selectors are honored; without one, delivery GETs derive the delivery's workspace and still check the user's membership, so relative package assets do not depend on another tab's selected workspace. Share management requires user authentication and an explicit `x-steve-ws` header. User access clients are supported; Bot session/MCP/Node tokens cannot authenticate to these REST actions, and no sharing tool is exposed to Bots. Setting `expires_at` or mentioning permission in a Bot message cannot create a public grant.

Public reads return 404 for unknown, expired or revoked tokens. Successful responses use `Cache-Control: no-store` and `Referrer-Policy: no-referrer`; proxies must honor these headers and must not cache the API paths. Revocation and expiry stop subsequent requests, including CSS and images, without restarting the Hub. Bytes already downloaded, displayed or in an already-authorized in-flight response cannot be recalled. Keep share URLs private to their intended recipients: possession of an active token grants access to that entire delivery, including its original archive.

At startup, the Hub repairs old output records that point to a prose summary when the full saved text can still be found. Before it changes a record, it creates a verified `pre-delivery-recovery-*.sqlite3` copy in `<home>/backups`. The repair is safe to retry after a failed start. Files that were deleted before the upgrade cannot be restored this way.

### Generic connector extension

Connector operations are account-bound entries in the unified tool registry. Agents discover them with `steve_tools_search` (`source:"connector"`, a connector ID, or `connector:<id>/<connection>`; optional resource/read-write filters), then use `steve_tool_call`. Pin an exact tool name on a Bot to include it in the session catalog; unpinned tools remain discoverable. Names are generated by the Hub from the connection and operation, not chosen by the plugin or model. Each call resolves the authenticated session principal, honoring a Node-bound session's recorded identity, and rechecks live workspace membership, account access, connection capabilities and target visibility. A later change to the item's requester cannot lend the session another person's account; service sessions and departed members never inherit the workspace owner’s personal accounts. The registered connector-reference migration stores expiring references; explicit searches/reads do not schedule history backfills.

A service may include `connector: {version: 1, id: "example", operations: [...], events: ["issue.updated"]}` in its ext_ws v1 hello. A connector needs only `me` and `changes`; messaging history/media/send features are independent, not mandatory. Installed `kind: "connector", contract: 1` plugins use the same per-account, supervised process contract as channel plugins. Platform-specific authentication, API calls and format conversion stay in that process, not in the Hub. No existing built-in platform adapter is moved by this extension.

Each operation declares standard MCP `name`, `description`, `inputSchema`, `annotations` plus `capability` (`search`, `history`, `read`, `write`) and `bindings`. Schemas must be closed objects (`additionalProperties: false`). An annotation's `readOnlyHint: true` is required for reads; `steve.resources` and `steve.asYou` describe resources and acting identity. Write operations must require a bound target. Discovery is not an authorization grant. The Hub supplies deterministic tool names for each connection, resolves Hub conversation/message/topic/reference IDs under the current session's workspace and account visibility, validates arguments, then sends `connector.call {name, arguments, expected_sender?, expected_identity?, idempotency_key?}`. Self write access uses `msg.send_as_user`; bot write access uses `msg.send` and requires the operation to explicitly declare `steve.asYou: false` (writes default to as-you). Operations declaring as-you are never offered or dispatched on bot connections. Both honor the current remote write policy; this does not replace Steve's separate outbound message delivery pipeline. A cached reference cannot authorize another account or a revoked/disconnected connection.

A call may return `references: [{kind: "reference", external_id: "opaque-provider-id", conversation_id: "project", source_message_id: "message", original_type: "document", title: "Design", expires_ms: ...}]` alongside its content. The Hub stores a connection/account-bound mapping and replaces `external_id` with its own `reference_id`. Plugins cannot choose that ID. References retain source/type metadata, expire after a day by default (at most 30 days), and fail explicitly when expired or missing; callers must search/read again. A subsequent bound read fetches the original only when requested. Registering search/history results does **not** import messages, identify historical participants, or start historical automation.

Messages may use `dm`, `group`, `channel`, `thread`, `mail_thread`, `meeting`, `pr` or `issue`; IDs, reply/root/topic references stay opaque and stable within the connected realm. Participant updates (`type: "participants"`, `conversation_id`, `change: "added" | "removed"`, `members`) use the same primary handles and aliases as message senders. Attachment entries support image/file/audio/video/document/webpage/commit/log references; only checked title/name/size/MIME/HTTP(S)-URL metadata and an opaque provider key are stored, never inline bytes, HTML or document bodies. The Hub adds a stable reference ID and hides provider keys in presented messages.

Declared events arrive in existing cursor/ack batches as `{type: "event", event_type: "issue.updated", event_id: "stable-id", occurred_ms: ..., summary: ..., conversation_id?: ..., topic_id?: ...}`. The ordinary Hub event sink determines privacy from the connection, deduplicates stable external event IDs even across reconnects, and routes matching automation. Remote `space_id`/`message_id` fields are never trusted Hub bindings. The connector mode starts at now, resumes from its saved cursor and **does not** automatically repair gaps by fetching history. History/search are explicit operations only. Legacy channel services without the extension retain their existing gap-repair behavior.

A write is sent once. If the response is lost or times out, its result is `status: "unknown", retry: false` with the request identity; do not automatically replay it. Reconcile against the upstream state before deliberately requesting another write. Capability removal and declaration changes take effect on the next dispatch. The full wire contract and fake-websocket regression vectors live beside the adapter, as linked above.

## Realtime output statistics

Poll `GET /api/stats/realtime` once per second with the same sign-in and `x-steve-ws` workspace header as other APIs. Workspace membership is checked by the API middleware; optional `project` and `bot` references must belong to that workspace, otherwise the response is 404. Together they form an intersection. Historical `range`, `from`, `to`, `hour`, time zone, model and source parameters have no effect. A workspace query parameter cannot switch the authenticated workspace. Responses use `Cache-Control: no-store` and bypass the historical statistics cache.

The stable response fields are:

```json
{
  "at": 1800000000000,
  "window_ms": 10000,
  "step_ms": 1000,
  "tps": 2.5,
  "output_sessions": 1,
  "state": "estimated",
  "coverage": {"measured": 0, "estimated": 1, "unavailable": 0},
  "series": [{"at": 1800000000000, "tps": 2.5, "state": "estimated"}]
}
```

This example shows only one series entry; an actual response has 60, oldest first, from `at - 59000` through `at`. All times are Unix milliseconds. The server rounds `at` down to a second. Each point sums output tokens in `(at - 10000, at]` and divides by ten seconds, including quiet portions of the window. `output_sessions` counts unique sessions with positive output in that same window, so it includes the tail after output stops; it is not a count of processes currently executing. Attempts and retries of the same session count once. Read-only imported sessions, background tasks, gateway calls and research jobs are outside this session-output metric. Attribution to workspace, project and Bot is captured at the first collected event, normally the attempt start; for a mid-attempt upgrade it cannot reconstruct earlier attribution.

| State | Meaning |
| --- | --- |
| `measured` | Reserved for verified streaming output-token measurements. None of the current built-in collectors produces this state. |
| `estimated` | Complete available session coverage using the documented text estimate, with output in the window. |
| `idle` | Complete available coverage with no output in the window, or no relevant sessions; TPS and session count are zero. |
| `unavailable` | Relevant execution has no usable collection evidence; TPS and session count are `null`. |
| `partial` | Some output or covered sessions are known, but coverage is incomplete; total TPS and session count are `null`. |

`coverage` counts relevant unique sessions, not samples or attempts. The three categories are disjoint: any missing attempt makes its session unavailable; otherwise a collected session is estimated. A session is relevant when its dispatched/started attempt intersects the ten-second window, including an attempt whose end is unconfirmed. The extra `estimator: "text-v1"`, `estimated_tps`, `measured_tps` and `observed_output_sessions` fields expose the known evidence separately. In a partial response, `estimated_tps` is only the observed portion, never the total; `measured_tps` is currently always `null`. Consumers must retain the state label and must not substitute the observed portion for an unknown total or silently combine measurement methods.

All registered harnesses use `text-v1` when they expose compatible assistant/thought text. No adapter currently enables `measured`. Claude Code and Codex's `usage_update.used/size` describes context occupancy; their separate output counts and Codex's rollout fallback are completed response accounting. Pi's inspected ACP bridge accumulates usage at `message_end` and returns `promptUsage()` at prompt completion. The Normalizer also accepts empty or nonempty `agent_*_chunk._meta.usage` as billing `Final` facts and subtracts repeated prompt totals; attaching a bill to a chunk does not establish when its output tokens were generated. Context size, chunk count and final output usage never enter realtime token samples.

The current registry contains `claude-code`, `codex`, `pi`, `gemini`, `github-copilot-cli`, `opencode`, `qwen-code`, `goose`, `kimi-code`, `grok-build`, `kilo`, `factory-droid`, `codebuddy-code`, `cursor`, `kiro`, `mistral-vibe` and `qoder`. The static usage audit covers Claude ACP 0.107.0, Codex ACP 2.6.0 and the public Pi ACP 0.1.8 source. It does not certify other versions or claim model-turn verification for any of them. Kimi's installed binary exposes no inspected adapter source; the other registered CLIs were not installed in the audit environment. Installing, discovering or registering an adapter never grants measured status. Custom manifests require their own usage audit; compatible text still uses the same explicit estimate.

`text-v1` counts each ASCII Unicode scalar as 0.25 token and each other Unicode scalar as one token, without rounding per chunk. This additive estimate is independent of network chunk boundaries. It includes exposed assistant and thought text, and excludes tool arguments/results, images, hidden reasoning and unexposed provider subagents; it is not a model tokenizer or a reconstruction of billing totals.

Collection runs inside the transaction that applies ordered Node events. `realtime_output_attempts` stores attribution, effective start/end boundaries and a separate `baseline` flag: `1` means collection observed the attempt start; `0` means it first saw a later event after an upgrade. A missing record also means no baseline, even when the Hub has marked the attempt finished. `ended` is written only by a Node `AttemptEnded`, independently of the baseline flag. A mid-attempt upgrade can therefore record a later Node stop without claiming to have collected the earlier output. Observed suffix text may contribute to the separately labeled estimate, while the total remains unknown until the incomplete attempt's tail leaves the window.

`realtime_output_samples` stores the source event id, Node/stream/epoch/sequence, `source_at`, collection time `received_at`, effective `at`, kind and integer quarter-token `units`; `realtime_output_blocks` keeps each message/block's UTF-8 byte position. Stream watermarks and block positions prevent replay or overlap from counting twice, including after a restart. Samples survive ordinary Node inbox pruning and are retained with the session (no independent telemetry expiry or new session-cleanup command). Back up the database as usual. Each poll reads a bounded 69-second sample range for the 60 overlapping windows, plus one indexed predecessor for freshness; a partial index finds the first gap even when it is older than the trend. Pending inbox events and faulted streams have separate indexed lookups. These reads do not discard historical samples or replay cursors to achieve their bounds; storage still grows with retained events.

To audit the observed estimate on a database copy, bind the response's millisecond `at` and authorized workspace to this query; use the attempt's `project_id`/`bot_id` for the same optional filters:

```sql
SELECT r.session_id, SUM(s.units) / 40.0 AS observed_tps
FROM realtime_output_samples s
JOIN realtime_output_attempts r USING (attempt_id)
JOIN sessions current_session ON current_session.id = r.session_id
 AND current_session.workspace_id = r.workspace_id AND current_session.readonly = 0
WHERE r.workspace_id = :workspace AND s.at > :at - 10000 AND s.at <= :at
GROUP BY r.session_id;
```

Final usage contributes zero units and cannot create an end-of-turn spike. Positive final output usage with no collected text is missing coverage, not an idle window. Valid late samples and stops retain their source time, so rereading a trend can correct earlier points without moving a backlog into the present. A nonpositive source time, a clock that moves backwards within an attempt, or one over one second ahead of receipt produces a zero-unit gap, retaining the original `source_at` for diagnosis. Its effective `at` is the later of receipt, attempt start and the last collected timestamp. An invalid Node stop uses that same effective boundary for `ended`; it cannot hide recent output or discard its own gap prematurely. An upgrade also repairs earlier persisted ends that precede collected samples, retaining their source timestamps.

Missing message baselines, UTF-8 gaps, dropped-event diagnostics, faulted/pending stream prefixes and uncertain runtime attempts are unavailable coverage. Gaps remain conservative for that attempt. An open attempt with no source event in the last three seconds also becomes unavailable, even if its stored status says running; this timeout is not proof that the model stopped. Fresh non-output events can establish an idle visible-text window for a complete baseline. Once the Node confirms an end, the existing output tail ages out normally: a last sample at `t` is absent at `t + 10000`. An incomplete or gapped attempt remains unknown through its end tail and stops affecting the current point at `ended + 10000`; its last unknown trend window leaves the 60-point view at `ended + 69000` (evaluated on second-aligned points). With no other missing attempts, those points become `idle` with zero TPS and zero output sessions. Earlier unknown windows remain unknown until they leave the trend.

A Hub-only settle to `finished` does not replace a trustworthy stop, including for attempts without any realtime baseline. During upgrade, applied `AttemptEnded` inbox records are matched by Node, session, turn and attempt and materialized once as `baseline=0` with `stop_evidence=node_event`. Their receipt time conservatively bounds the unknown interval, regardless of the old source clock. Matching `start_turn` terminal receipts can instead establish `stop_evidence=node_receipt`; receipt payload operation, terminal state/outcome, command Node and target attempt must agree. Their receipt timestamp bounds the interval, never a bare attempt status or `updated`. The same receipt rule handles subsequent incomplete attempts. Existing complete collection baselines continue to rely on ordered Node end samples, so an early receipt cannot discard in-flight text.

A rejected `start_turn` with `maybe_sent=0` and a persisted pre-send cancellation reason or refusal records `stop_evidence=unsent,not_started=1`; it contributes no unknown execution interval. A rejection without that evidence or with a possible dispatch remains unknown. With neither stop nor unsent proof, missing coverage has no inferred expiry: another attempt or a Hub status update cannot prove that an orphaned execution stopped. A delayed Node stop can resolve the boundary. Stop evidence is stored independently of ordinary inbox pruning; polling does not rescan old inbox history to recover it.

Legacy non-Node sessions have no such collector; when their persisted lifecycle becomes `ended`, its `updated` time bounds the uncollected interval conservatively. This lifecycle boundary is not a measured stop or a retroactive baseline: every overlapping historical window stays unknown, then leaves the current window and trend after the same ten/69-second tails. Read-only imports remain excluded.

## Workflow statistics and a baseline

`GET /api/stats/summary` carries a `workflow` object, shown on the statistics page as **工作流**. It counts, for the range and the project and Bot filters (applied through the work graph's own item):

- `attempts`, `retries` (unit attempts that were not the unit's first, also `quality.unit_retries`, shown beside the acceptance rework count `quality.reworks`), `attempts_per_unit`, `resets` and `first_try` (by unit kind: first attempts that delivered and passed; `first_try.review` is the first-round review pass rate);
- `sessions_per_item` (sessions of an item done in the range and of its work units), `coordinator_tokens`, `unit_tokens`, `coordinator_share`, `coordinator_turn_ms` and `coordinator_long_turns` (turns longer than `long_turn_ms`);
- `wakes` (coordinator wakes about a settled unit, by unit status), `invalidations` (closings invalidated, by reason), `merged` and `accept_to_merge`, `rejected`, `rejects` (by reason), `delivered` and `reject_rate`;
- `reply_latency` (a channel message stored → its first reply sent; workspace only) and `first_tool` (a session's start → its first tool call; `no_tool` made none).

These read item marks recorded since this release: `invalidate`, `reject`, `merge` and `wake` (its detail has `unit_id`, `status` and `attempt`), and `sessions.first_tool_at`. The upgrade turns earlier closing invalidations and pull request merges in item timelines into marks once; sessions from before take their first recorded `tool_call` event.

To take a baseline without a running Hub on the data, print the same figures from a database file:

```sh
steve-hub --stats-snapshot --db <copy of the database> [--workspace <id>] [--from YYYY-MM-DD --to YYYY-MM-DD] [--scratch <dir>]
```

It prints JSON with `workspace`, `schema`, `range`, `workflow`, `quality` and `throughput`, for the last 30 days of the workspace's statistics time zone unless a range (at most a year) is given; with several workspaces, name one. The file is opened read-only and never changed. A database from an older release is copied with SQLite's online backup into a temporary directory (`--scratch`, else the system's) and the copy migrated first, so the scratch location needs room for one copy; the copy is removed afterwards. Run it on a copy made with `sqlite3 <database> ".backup <copy>"` (or a daily backup) rather than on a live installation's file, and keep the output with the release it was taken on to compare later ranges against.

## Plugins

A plugin is something that is not part of Steve, installed from a plugin library: a program run beside the Hub as a process of its own, or data the web client reads. It is reached through one of Steve's documented contracts and never links into the Hub. There are three kinds: a connector plugin, a generic event/read/write service described below; a channel plugin, a [channel service](#an-external-channel-service) the Hub starts and connects for you, and a theme plugin, a skin for the web client that is only data.

**Using them.** Plugins are in Settings → Plugins (`#/settings/plugins`): first anything that needs attention (a connection whose plugin is not running, a library that could not be read), then what is installed, what the libraries offer, and the libraries. Plugins sit under Settings rather than beside Capabilities because they are the instance's, installed once by its owner for everyone, while Capabilities are what each Bot is given. The instance's owner adds libraries and installs, upgrades and uninstalls plugins; anyone signed in connects an installed plugin with their own settings (an address, a token, whatever that plugin asks for). Connecting starts one process for that person and creates an external-channel connection of theirs, which then appears in Accounts like any other. The connection's token is kept in the vault, so the vault has to be turned on first. Disconnecting it in Accounts stops the process and removes its settings and data. A library with something installed from it, and a plugin somebody is connected through, cannot be removed until that is undone.

**Trust.** Installing a plugin runs its program as the Hub's user, outside the sandbox the Node gives agent runs. A library is trusted as much as the owner trusts it by adding it; what is installed is exactly the archive whose sha256 the library's index names. The official library, `https://github.com/Infranite/steve-extensions`, is listed on a new installation and nothing is read from it until the owner asks. `STEVE_PLUGIN_LIBRARY` names another first library, or `off` for none; once the owner has changed the list, the list is what they made it.

**A library's address** is one of:

- `https://github.com/<owner>/<repo>` (or `<owner>/<repo>`): `steve-store.json` at the root of the repository's default branch;
- the `https://` address of an index file itself, on any host (plain `http://` only on `127.0.0.1` and `::1`);
- an absolute directory on the Hub's machine with `steve-store.json` in it: a library in development, or one that is not published.

**The index**, `steve-store.json` (format 1; unknown members are ignored):

```json
{
  "store": 1,
  "name": "Example plugins",
  "plugins": [{
    "id": "example",
    "name": "Example",
    "summary": {"en": "What it does", "zh-CN": "它做什么"},
    "kind": "channel",
    "version": "0.1.0",
    "contract": 1,
    "entry": "bin/example",
    "homepage": "https://example.org/plugin",
    "config": [
      {"key": "address", "label": "Service address", "hint": "wss://…"},
      {"key": "token", "label": "Access token", "secret": true},
      {"key": "note", "label": "Note", "required": false}
    ],
    "artifacts": {
      "aarch64-apple-darwin": {"url": "https://example.org/example-0.1.0-aarch64-apple-darwin.tar.gz", "sha256": "…"}
    }
  }]
}
```

- `id` and a setting's `key` are a lowercase letter or digit followed by lowercase letters, digits, `-` and `_`, at most 40 characters. `name`, `summary`, `label` and `hint` are a string or a map from language to string.
- `version` is a semantic version. The index names one version of each plugin, the one to install now; an installed plugin with an older version is offered as an update after the owner reads the library again.
- `identity` selects the channel/connector connection provider: `"self"` (the default) or explicitly `"bot"`. It must agree with the service's hello and account identity declarations; it never changes an existing account's identity.
- `config` lists the settings a person enters when connecting. A `secret` one is entered like a password and never shown again; a setting is required unless it says `"required": false`.
- `artifacts` is keyed by the target the Hub was built for (`steve-hub --build-info` prints it). A plugin without an artifact for the Hub's target is listed and cannot be installed there. `url` is `https://`, or a path relative to the index, which is the only form a directory library uses. The archive is a `.tar.gz` of regular files and directories, at most 256 MiB; an archive with a link or a path that leaves it is refused. `entry` is the executable in it.
- `kind` and `contract` say how the Hub runs the plugin: `channel` or `theme`, each at contract 1. One whose kind or contract this Hub does not know is listed as needing a newer Steve.
- An artifact keyed `any` is used on every target that has none of its own; a theme, which is data, usually has only that one.

**A channel or connector plugin** (`"kind": "channel"` or `"kind": "connector"`, `"contract": 1`) implements the external channel protocol (`hub-rs/crates/steve-channels/src/adapters/ext_ws/protocol.rs`) for one signed-in person or bot. The Hub starts its `entry` once per connection, with `PATH`, `HOME`, the locale, `TMPDIR` and `TZ` of its own environment and these variables:

| Variable | Meaning |
|---|---|
| `STEVE_PLUGIN_LISTEN` | `127.0.0.1:<port>`: where to serve the protocol, with the WebSocket at `/ws` and media under `/v1/media/` |
| `STEVE_PLUGIN_TOKEN_FILE` | A file with the bearer token the Hub presents; refuse anything else |
| `STEVE_PLUGIN_CONFIG` | A JSON file: an object with the person's settings, each a string under its `key` |
| `STEVE_PLUGIN_DATA` | A directory that is the instance's to write; it is also the working directory |
| `STEVE_PLUGIN_SUPERVISED` | `1`: the Hub holds standard input open; exit when it reaches end of file, which means the Hub is gone |

The plugin has 20 seconds to start listening when it is first connected. One that exits is started again after a pause that grows from a second to a minute; meanwhile its connection behaves like any external channel whose service is unreachable. To stop it the Hub closes its standard input and sends `SIGTERM`, and kills it after five seconds.

**A theme plugin** (`"kind": "theme"`, `"contract": 1`) is a skin for the web client. Nothing of it runs, on the Hub or in the browser: its `entry` is a [theme manifest](#appearance-and-themes) in the archive, a JSON object of at most 64 KiB, and the Hub refuses to install an archive whose `entry` is not one. It has no `config` and is not connected. Once it is installed the web client reads the installed manifests (`GET /api/plugins/themes`, for anyone signed in), checks each one and registers it, and the theme is a choice of its kind in Settings → Appearance for everyone on the Hub, on each of their devices. A manifest the client refuses is shown as refused on the Plugins page, with the reason. Uninstalling it removes the choice; a device that used it goes back to the default.

```json
{
  "id": "ocean", "name": {"en": "Ocean", "zh-CN": "海洋"}, "summary": "A blue accent",
  "kind": "theme", "version": "1.0.0", "contract": 1, "entry": "theme.json",
  "artifacts": {"any": {"url": "dist/ocean-1.0.0.tar.gz", "sha256": "…"}}
}
```

**On disk**, everything is under `<home>/plugins` and readable without a running Hub:

- `stores.json`: the libraries and the index last read from each;
- `installed/<library>/<plugin>/`: `installed.json` beside the unpacked version;
- `instances/<id>/`: `instance.json`, the settings (`config.json`) and `token`, both readable by the Hub's user only, the plugin's `data/`, and `plugin.log` with what it wrote to standard output and error (started again once it passes 1 MiB). Look there first when a plugin does not start.

## Usage accounting and recovery

The session panel, an item's usage rows and the statistics views read the same `usage_records`
projection. Node attempt reports are durably deduplicated in `usage_reports` by `(turn_id, report_id)`;
`usage_meters` keeps the current head of each `(turn_id, meter_epoch, model, scope)`, including accounting finalization and invalid-amount state. The durable ledger and local/Node one-shot calls use the same `steve-node-api::usage` reducer. Applying a Node
report, replacing its accounting projection, updating the daily cube, invalidating the item cache
and advancing the applied event watermark share one transaction. Statistics write failure leaves
the event unapplied for retry. This does not require a source checkout, model connection, or an
operator's private infrastructure.

The existing `UsageReport` contract is used throughout:

- `delta` adds the report's known fields once. `cumulative` is the snapshot of this attempt's meter,
  replacing earlier known values. `final` replaces the running figure and prevents later non-final
  reports from increasing that meter. A missing field stays unknown unless an earlier report of
  that meter already supplied it; it is never manufactured as zero.
- A new harness generation uses a new `meter_epoch`. Models and attempts have independent meter
  heads. For the same epoch and model, `all` supplies each known field and `main`
  supplies missing fields; a main-only dollar quote is retained when `all` has no quote. A cost/context-only `all` report does not
  replace known tokens with zero. `reasoning` is already part of `output` and is not added again.
- Live attempt usage is assigned to the attempt's start time and that day's workspace `stats.tz`.
  Finalization leaves that attribution in place. Turn durations and completion counters are still
  booked on the completion day. A time-zone rebuild recalculates both from their timestamps.
- Cancellation or interruption closes the turn and retains its known usage. It does not mean the
  provider reported a complete final bill. A duplicate finish cannot append another charge.
  A late final can repair a settled attempt without counting its completion again.
- One-shot gateway calls use the same report reducer before recording their model rows. Explicit
  zero reports are retained as row or detail evidence, without adding requests for unused models.
  A single attempt's zero-model evidence contributes to coverage, including model filters; retry
  zeros cannot stand in for the answering model's missing usage. This path does not estimate a live rate:
  cumulative or final billing figures must never be divided into a recent TPS window. Older
  context-only ACP process-cost meters are rebased to admitted attempt increments at the Hub
  boundary; their context figures are not token-usage reports.

All/Main selection is per epoch and reported model, field by field: an All output does not
remove a Main input that All omitted, and an All for model B does not cover model A. Background
reports are outside the attempt. An anonymous Main from the adapter's prompt response can use the
epoch's single All model as its canonical owner; requested aliases are only display fallbacks,
not fuzzy matches between two explicitly reported names. If several All models exist, only uncovered anonymous
Main field residuals are retained, with no guessed canonical model. Different actual epochs remain independent;
collectors must give live and final observations of one meter the same epoch. A context-only Final
cannot finalize accounting. A valid accounting Final retains prior missing fields and ignores late
non-Final accounting. Invalid or mixed-currency delta amounts remain unknown until a valid snapshot;
foreign currencies are never silently booked as USD. Remote session cost admission still applies
its configured caps separately from the accounting projection.

New gateway records carry `detail.call_id`; turn records use their stable turn ID. A legacy Main
cost-only row with a matching All token row and stable turn/call ownership is a cost companion,
not another missing-token call. A same-millisecond peer without stable ownership is only a
candidate: `coverage.uncertain` counts it and the UI says historical cost ownership is uncertain.
It does not increment `unavailable` or certify complete coverage. An independent cost-only call,
including one for a different model, still has unavailable token usage. Rebuild preserves this
classification; it does not manufacture call IDs for historical rows.

`GET /api/stats/sessions/:id`, `/api/stats/items/:id`, `/api/stats/summary` and usage breakdowns
include `coverage`. Its `state` is `not_reported`, `unavailable`, `partial` or `complete`:
an open attempt with no token report is not reported yet; a closed attempt without tokens is
unavailable; any incomplete field or uncovered attempt makes a known sum partial. Complete means
all four accounting token fields were reported, not that a run has ended or every provider feature
is observable. `coverage.total` and the per-field values are `null` when unknown and `0` only for an
observed zero. For compatibility the original numeric fields remain additive sums of known values;
consumers must use coverage before presenting those sums as totals. Coverage counts meter/model
rows and attempts with no rows, not user prompts. Reads of an accounting response use one database
snapshot. Estimates still depend on the configured price table; coverage is separate from pricing.

After an ordinary disconnect or Hub restart, reconnect the existing Node. Its durable event replay
uses the same report identities; retained meter heads and usage rows remain available while the
model is offline. Never start a replacement attempt just to recover statistics. A completed attempt
whose old usage is still in the Node inbox is repaired when its finish reports are processed.
Owner-only `POST /api/stats/rebuild` reconstructs daily and item summaries from persisted usage and
turn rows (also after a statistics time-zone change); it does not contact a provider or recreate
reports that were never collected. Keep normal database backups. Historical missing usage whose
source records were never emitted or have already been discarded remains unavailable; neither text
length nor a bill for another attempt is a valid substitute. Collector-specific import/backfill is
a separate integration and must provide stable report identities and attempt attribution.

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

### Concurrent sessions and capacity queues

Every session-capable Node, including the Hub's own in-process or service Node, starts with **32 concurrent turns**. In **Devices → Nodes → this machine's Node → Capabilities and admission configuration**, the instance owner can save 1–256 using the current configuration revision; for a paired Node, its owner uses **Devices → Nodes → that Node → Concurrent sessions**. This is resource scheduling, not a new approval or harness permission. It applies to every harness through the same Node contract. Idle/open sessions do not use turn slots; model jobs and command executions keep their separate capacities.

The setting is persisted on the Hub (`devices.max_sessions` for its `hub`/`local` machine, `nodes.max_sessions` for a paired Node). A save needs no restart: the Hub uses it on the next dispatch, normally within one second when its sender is idle. Raising it releases eligible queued turns; lowering it does not interrupt accepted/running turns and starts no more until the count drops below the new setting. The Hub reapplies the setting to the Node's actual limiter before each turn dispatch, including after a Node reconnect or restart. Offline or paused Nodes still wait for their normal link/resume; saving capacity does not connect, resume or move work.

Excess turns stay as the **same durable operation/attempt** on the Hub in ready-command order and start automatically as slots free. Preparation dependencies must be complete before a turn counts as waiting for capacity. In-flight sends and operations with unknown outcomes still occupy a slot until reconciled/settled: an uncertain send is not permission to start a duplicate. Stop, permission answers and other control operations can pass waiting starts. Devices shows the Node-wide running and queued counts (across workspaces); the conversation's live status says when the device is full and shows running/maximum. A definite Node refusal instead shows its actual reason and retries automatically, rather than calling storage exhaustion a capacity wait.

Nodes speaking a protocol older than 0.4 retain their existing executor limit; the Hub still queues against its saved setting, but cannot raise an older executor's limit remotely. Update the Node to the same release as the Hub to apply live capacity changes at both ends. Node refusal details remain visible while waiting. On upgrade, legacy device rows still at the old display-only default of 4 become 32; other saved values are retained. Newly paired Nodes start at 32.

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

3. Once the Node claims the code, **Add device** moves on to its fingerprint (a pending Node also has **Confirm** on the Devices overview). Paste or type the whole fingerprint the CLI printed, eight groups of four hex digits: each group is checked as it is entered, and **They match, add it** is available only when all eight match. If they differ, **They differ, reject** unpairs it; make a new code and pair again. Save its `node_id` for subsequent checks; do not identify it by name alone. The API equivalent is to read `GET /api/nodes`, match the Node ID and fingerprint, then call `POST /api/nodes/<node_id>/confirm` and check for `{"ok": true}`.
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
Any ACP adapter can be a harness. Besides the 17 built-in entries ([Harness setup](#harness-setup)), an entry can declare an adapter of its own: `{"my-agent": {"command": "/absolute/path/to/agent", "name": "My Agent", "args": [], "env": {}, "properties": {}}}`. Only `command` is required; sessions use plain ACP. `properties` declares what plain ACP cannot discover, for example `modes` (Steve's `plan`, `ask`, `accept_edits`, `auto` and `full_access` mapped to the adapter's mode ids; by default `plan`, `default`, `acceptEdits`, `auto` and `bypassPermissions`), `blind_set_mode`, `mode_meta` and `native_execution` (native tools without harness approvals, FullAccess only), `nested_sandbox: false` for an adapter that sandboxes itself and must not run inside the Node's sandbox, `self_read_only`, and `skills` (where under its state directory it reads `<name>/SKILL.md`; see [Skills](#skills)). The Node asks each such adapter `initialize` (at most every five minutes; `probe_initialize: false` turns this off) and reports its version, capabilities and sign-in methods, or why it could not start. The Hub reads the same format from `$STEVE_HOME/harness.json` (or `STEVE_HARNESS_MANIFEST`) for its own Node; an adapter listed there is offered as a bot's harness. `STEVE_ADAPTER_*` variables still take precedence for the built-in entries, and a manifest that does not parse is logged and ignored.

Registration alone does not move existing projects or change scheduling. Do not set `STEVE_NODE_BIND` merely to register a Node. The current remote Node configuration advertises session capacity; jobs and execs have zero capacity.

### Durable one-shot bills

Think calls and research jobs allocate a durable `gateway_calls.call_id` before dispatch. Each fallback allocates its own `gateway_attempts.attempt_id`, ordinal and `previous_attempt`, even when it retries the same model. Workspace, item, purpose, bot and project are captured before work starts; model and harness are captured before each attempt. Research retains every attempt's consumption. A non-model Node operation does not allocate a model bill.

Node jobs remain `ExecutionRef::Job`. Their dispatch transaction links the Job attempt to the billing attempt in `gateway_job_accounts`; it does not create an agent turn. Applying Usage writes `gateway_usage_reports` (deduplication key: attempt/report), shared-reducer meter heads, the replaceable `usage_records` projection and `stat_daily`, and the inbox applied cursor in one transaction. Report identities and the source node/job binding survive normal Job/inbox pruning, so a retained late Final still repairs the original bill after its runtime Job row is gone. This is a Hub storage contract: the existing `JobSpec`, `UsageReport` and old Node messages remain readable without new mandatory protocol fields. Older jobs without this persisted ownership cannot be assigned an item retrospectively from a timestamp.

LocalAcp first durably captures reports in `gateway_local_inbox`. Applying one report atomically commits its report identity, meter, bill/cube and applied flag; a projection fault leaves the captured report pending. The Hub apply worker and startup recovery retry it without a caller, in attempt order; a failed cumulative report cannot be overtaken by a later delta, and unrelated calls continue. Caller results are durable facts with a projection-dirty flag, so an unavailable projection cannot erase a timeout/cancel result.

Finish and cancel share a separate persistent FIFO of dirty call results. Each worker pass selects at most 128 results and commits their rotation to the tail before attempting projections. Projection rollback does not undo rotation; successful projection atomically clears dirty state and its queue entry. Ticket ordering survives restart, and new results join behind existing entries. Concurrent workers serialize selection/rotation; overlapping projections are idempotent and need no lease. For a queued call with N entries up to and including it, at most ceil(N/128) committed scheduling batches select it for an attempt, even when earlier calls keep failing or new calls arrive. This bounds scheduling work, not elapsed time or success while storage remains unavailable. Failed passes are logged and the idle worker retries without new Usage. Finished bill detail explicitly records `pending: false`; result recovery keeps the same call, consumption and failure outcome.

Core records the caller result and fallback metadata on the same call; it does not append a second bill. A timeout returns promptly, while Node cancellation and bounded sampler settlement continue. Dropping the future removes its text waiter, not its accounting owner. LocalAcp runs the same bounded settlement in an owned task and persists each report. Later Final reports replace that attempt's known cumulative values and may correct a failed call's consumption; they never turn the failed result into success. Attempts, duration and failure evidence stay on the first accounting row, and zero-consumption retries remain evidence without an extra request row.

A live Hub persists interrupted callers as failed before admitting work and attempts projection recovery, then reconciles Node jobs and applies retained inbox/journal events without running the models again. A projection outage is logged without preventing Hub startup, so the periodic worker can continue bounded retries. A failure to persist the interrupted caller results still prevents startup. An inert `STEVE_NO_RESUME=1` snapshot does not do this recovery. An already received Final is recoverable after process restart even when the original caller no longer exists. Node report event times (`NodeEvent.at`) are retained separately; LocalAcp has no event timestamp in `EventBody`, so it records the observation time, not an inferred original token timestamp. Billing uses the captured call start and workspace statistics day. Rebuild keeps that day in sync with timezone changes. Neither cumulative bills nor late or historical finals add current TPS units.

Think and Research stop through an awaitable one-shot cancellation path. User cancellation, deadline/offline/output limits and Research idle, tool, turn or policy stops cancel execution before awaiting source settlement. The adapter receives TERM, with at most 500 ms for its leader to exit, then KILL for its remaining owned group and at most two seconds to reap. No further prompt or tool permission is driven after cancellation; only shutdown and bounded source reads continue. After settlement, Node cleanup rechecks the recorded process birth identity; an uncertain or reused identity is not signalled and remains unknown. The same attempt/session/meter is sampled, including source bytes persisted before cancellation but not yet polled. Final is journaled on the original Job before AttemptEnded; a failed streaming Final is retried with the terminal transaction. Existing report identities make replay idempotent and retain the caller's failed/cancelled result. Source loss preserves observed consumption as a lower bound with a diagnostic, not a made-up zero or a presumed complete tail.

Rust callers that need an accounting receipt on stop use `steve_acp::oneshot::run_cancellable` and await its result. The existing `run` interface remains compatible for request deadlines. Dropping a raw ACP future is emergency process cleanup, not an awaited accounting receipt. Node-owned Think/Research lifecycles keep driving settlement when their Hub waiter disappears; shutdown still has its bounded emergency abort path, so abrupt process loss can preserve only reports already journaled.

This cannot recover usage a producer never flushed, a local source never observed before process loss, or events already pruned before ownership was recorded. Missing historic ownership, missing source bytes and ambiguous old meter boundaries remain unknown; do not infer zero, merge distinct epochs or add a supplier call to fill the gap. Use retained sources and trustworthy attempt boundaries for an explicit audit instead.

### Harness usage accounting

Persistent sessions and one-shot calls use the same token normalizer and attempt sampler. Native `session/prompt` usage and `_meta.quota.model_usage` are preferred for final accounting. Main-loop and all-model scopes overlap and must not be added together. ACP `usage_update.used` and `size` describe context occupancy, not tokens consumed; its reported cost can be a process cumulative meter independently of tokens. No token figures are estimated from message text. A missing report means unreported, a final report with absent token fields means unavailable, and an explicitly reported `0` is real zero. Some fields can be known while others remain unavailable.

Adapters declare `usage_input_includes_cache` and `usage_fallback` in `harness.json` properties. The built-in Codex adapter follows codex-acp 2.x: native input already excludes cached reads and writes; `thoughtTokens` in prompt usage and `reasoningOutputTokens` in quota are subsets of output. A legacy or custom adapter whose native input includes cache reads must declare `usage_input_includes_cache: true`. Claude's native input is exclusive of cache reads and writes; its live context/cost updates do not contain consumed token counts. An adapter with no native token accounting and no declared fallback reports unknown fields, including on errors.

`usage_fallback: "codex_rollout"` enables a read-only meter of the adapter's effective `CODEX_HOME/sessions` directory. The configured home is passed to the child; manifest, wrapper and principal state overrides are honored. Raw rollout input includes cached reads and cache creation when reported; both are split out once. Explicit `cache_write_input_tokens: 0` stays zero, a positive value stays separate, and an omitted field stays unknown. A baseline is taken before each prompt, then new `token_count` rows are sampled every 500 ms. Cumulative reports contain only this attempt's consumption, even when the session has older history. Native and sampled reports for the same attempt share a token meter; different meters are never relabelled to hide a discrepancy. Native final fields take precedence; sampling fills missing fields. Report ids derive from their content and attempt, so replay does not create another booking. A lower counter needs per-response `last_token_usage` where a trustworthy total difference cannot be obtained. Missing or inconsistent cache splits stay unknown; reasoning is never added to output again.

`usage_fallback: "claude_transcript"` (the built-in Claude adapter's default) reads assistant `message.usage` under the child process's effective `CLAUDE_CONFIG_DIR/projects`, or its `HOME/.claude/projects` when unset. This is a source capability, also available to a custom adapter under any name. Both ordinary and one-shot calls prime before prompting, exclude older timestamps and baseline message IDs, and deduplicate repeated content-block records by `message.id`. Session filenames and record session IDs must match; duplicate root files are ambiguous. Settlement accepts late disk writes only when their source timestamps precede the end fence, so padding after the attempt is not attributed. A new top-level user prompt bounds the previous attempt. Main-chain usage is reported by source model; session-bound files under `<session-id>/subagents/agent-*.jsonl` contribute to All without being added to Main. Input, cache reads and cache creation are already separate in this source. Synthetic assistant messages are ignored, and an all-zero native cancellation response cannot erase persisted consumption. Errors still settle the transcript when there is no native final response. This supports observed claude-agent-acp 0.99.x and 0.107.x source shapes; the actual adapter command and package determine which implementation runs. Their context/cost notifications alone do not establish consumed tokens, and a cancellation without a result can report synthetic zero.

Final collection allows three seconds for asynchronous tail writes after success, cancellation or error, plus at most 200 ms for its last read. The session stays occupied until settlement, and one-shot usage is journaled while the call runs. A recent earlier count is not proof that the final response was flushed. Reads stay bounded: at most 4 MiB to establish a baseline, 512 KiB per incremental read and 256 KiB per line. Codex discovery visits at most 16,384 entries; transcript discovery visits at most 4,096 projects and 128 files, with 65,536 message IDs per attempt. An unread or incomplete tail produces a diagnostic; previously observed consumption remains an observed lower bound, rather than being erased by a synthetic zero. Missing files and incomplete historical baselines never imply zero. Separate homes and session ids have separate cursors.

On Node shutdown, persistent-session actors abort and join their sampling coroutine, RPC continuations and retry/kill timers instead of leaving background work behind. Node-owned Think and Research jobs await their bounded stop and source settlement before shutdown returns. Each sampler permits only one blocking source read, and its async caller stops waiting after 200 ms; snapshots never wait for the reader's lock. An operating-system read on a stalled filesystem cannot be forcibly cancelled: that one worker can outlive shutdown until the OS returns, but holds no Node handle, home lock or event sender, and cannot schedule another read. Shutdown does not promise to flush an unobserved provider response. Cancelling a one-shot call stops its producer, so only already-persisted records and tails flushed before it stopped can be recovered.

This collection does not rewrite historical statistics. Existing Node journal reports can be replayed while retained. Historical rollout recovery requires the original configured home, exact session identity and trustworthy attempt boundaries or before/after totals; session lifetime totals or context meters alone cannot reconstruct a turn. Files deleted before collection, unflushed usage, writes beyond the bounded tail window, or resets without enough per-response evidence remain unknown. Abrupt Node loss may preserve only the last journaled cumulative report; it cannot prove an unflushed final count. Keep those limitations visible when auditing or repairing older data.

### Verify and recover a Node

`steve-node status` reports the saved pairing and fingerprint; it does not establish that the process is online. Verify Devices shows the same Node ID as enabled and connected, or that `GET /api/nodes` reports `state: "active"` and `linked: true`, and check that the Node's own log contains `linked to the hub`. A link attempt that fails for a reason other than the Hub being away (for example `internal`, such as a handshake message over its size limit) is logged by the Node at ERROR as `hub link failed, trying again`, and the Node keeps dialling. The Node notes the last failure in `link-status.json` in its home and removes the file once it links. Devices and `GET /api/nodes` show the last failure as `link_error` (`at`, `error`) while the Node is disconnected, and items whose input waits for that Node say `等待 Node 连接：<reason>`. The Hub reads its own Node's file. A remote Node tells the Hub instead: after such a failure it opens one more connection and answers the Hub's challenge with a signed `report` (its reason, at most 1 KiB) in place of a hello. The Hub checks the signature against the paired key, notes the reason and closes that connection without linking. The Node sends the report again when the reason changes and every 5 minutes. The Hub keeps the reason in memory until the Node links, so after a Hub restart it appears again with the Node's next report. A failure that keeps the Node from reaching the Hub at all (DNS, TCP, TLS, a certificate pin mismatch, a revoked Node) cannot be reported this way, so check the Node's log and `link-status.json` for those. After restarting only the new service, verify the same identity reconnects. Verify execution separately with a bounded test session when an ACP adapter and a project root are configured.

`GET /api/nodes` also says how each Node's latest link reached the Hub and what machine it is; both are kept while the Node is away, and `linked` says whether it is connected now:

- `link`: `route` is where the Node's machine is as the Hub sees it (`loopback`, `lan`, `tailnet`, `public`, or `unknown` for a tunnel or a proxy that does not say), `tls` whether it arrived over TLS, `via` what it came through (`direct`, `proxy` for a reverse proxy in front of the Hub, `tailcat` for its tunnel), `peer` the address it came from when known, `since` when the link opened, and `latency_ms` / `latency_at` the round trip of the Hub's latest call over it (the minute's vitals sample) while it is linked. Forwarding headers (`X-Forwarded-For`, `X-Real-IP`, `X-Forwarded-Proto`) are read only from the peers the Node routes admit, and only when sent once.
- `host`: what the Node said of its machine when it linked (`node.host`, protocol 0.7): `hostname` (on macOS the computer name), `os` and `arch` as Rust names them (`macos`, `aarch64`), `os_name` and `os_version` (`macOS` `26.1`, `Ubuntu` `24.04`), `cpus` and `mem_total` (bytes). A Node of an earlier release is not asked and shows `null`.

The event stream tells a Node's owner (and the Hub's owner) when anything of this changes, with `{"type": "node", "id": "<node_id>", "user": "<owner id>"}`: it asked to join, was confirmed, renamed, paused, resumed, revoked, linked or went away, its limit, personal-config choice or upgrade policy changed, an upgrade moved to another phase, it said what machine it is, or it reported why it could not link.

Fingerprints are 32 hex digits (128 bits of the key's SHA-256); `steve-node pair`, `status` and `rotate` print them in groups of four. Devices shows Nodes paired earlier at the same length, computed from their stored key.

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

### Upgrading Nodes from the Hub

A Node installed as a service from a release bundle (`steve node install`, or a Devices install over tailcat) can be upgraded by its Hub to the Hub's own release. The Node says so in its probe (`node-upgrade-v1`) with its version, commit and target; the Hub decides from that, never from a version number. A Node run by hand, a development build or a Node from before this release shows "upgrade on its own machine" in Devices and keeps the manual procedure above.

Each Node has an upgrade policy: `auto` (the Hub upgrades it on its own when they differ, for example after the Hub itself was upgraded), `notify` (Devices shows it) or `off`. A Node without its own follows the Hub's default, `notify` unless the instance owner changes it under **Devices → Hub → Defaults for every Node** (`PUT /api/nodes/upgrade-default`; its `GET` also returns `drain_secs` and `health_secs`, the waits below, which a Node's Version card quotes). The instance owner can always start one with **Upgrade to the Hub's version** (`POST /api/nodes/<node_id>/upgrade`). An automatic upgrade that failed is not tried again for the same Hub version.

1. The Hub prepares the archive under `<data>/node-upgrades`: for a Node of the Hub's own platform, a bundle of the Hub's own `release.json` and `bin/steve-node`; otherwise the published installer archive for the Node's target, checked against the release's `steve-installer.json` (size and SHA256; `STEVE_NODE_INSTALLERS` names a local directory for a Hub without the internet).
2. The Node fetches it over its authenticated link, granted only to that upgrade's operation and that Node while it stages, checks its size and SHA256, unpacks it (plain files and directories only), and requires `release.json` and `bin/steve-node --build-info` to report the offered version, commit and target. It is kept as `<node-home>/releases/<version>` beside the current release.
3. The Hub gives the Node no new turns and waits for its running turns to end (`STEVE_NODE_UPGRADE_DRAIN_SECS`, default 1800). If they have not, the upgrade is given up and the staged release removed; **Upgrade now, cutting running work** goes ahead instead, and the turns are continued as after any Node restart.
4. A helper outside the Node's service (its own session; its own `systemd-run --user --scope` under systemd) stops the service, points it at the new release as `steve node update` does, starts it and waits for the new Node to link to the Hub (`STEVE_NODE_UPGRADE_HEALTH_SECS`, default 180, set on the Hub). If it does not, the helper switches back to the release it came from and reports `rolled_back`.

Both sides record every step before taking it: the Hub in `node_upgrades` (shown as the Node's last upgrade), the Node in `<node-home>/updates/node/<operation>.json`, the helper's log in `<node-home>/logs/node-upgrade-<operation>.log`. A lost answer is asked about, never sent again; after a Hub or Node restart an unfinished upgrade is settled from the other side's state, not run on. The previous release stays on the Node for a manual rollback with the procedure above.

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

When a Node cannot reach the Hub over HTTPS (no public address, no VPN), the Hub can be reached over [tailcat](https://github.com/tailscale/tailcat), an optional program from Tailscale that tunnels TCP between two machines without an account or root. Install `tailcat` on both machines (`STEVE_TAILCAT` names the program when it is not on the service's `PATH`). The owner then chooses **Through tailcat** in **Devices → Add device** and turns it on there (or with the switch on **Devices → Hub**).

The Hub then:

- makes a key once in `<hub-home>/tailcat/` so that its address stays the same across restarts;
- listens on `127.0.0.1:<port>` with the Node routes (`/node/pair`, `/node/ws`) and nothing else; the port is `STEVE_TAILCAT_PORT`, by default the Hub's port + 2;
- runs `tailcat serve <port>` as its own child, starting it again if it exits and ending it when the Hub stops.

Devices shows the resulting address, `tailcat:<address>:<port>`. Use it wherever an HTTPS Hub URL goes: `steve setup node --hub tailcat:<address>:<port>`, or `steve-node pair tailcat:<address>:<port>`. The Node runs `tailcat forward` itself and dials its loopback end. The tunnel is WireGuard between the two machines, and the address names the Hub's key, so no certificate or pin is involved. Treat the address like a credential: it does not grant access by itself, because pairing still needs a one-time code and the fingerprint is still confirmed, but it is what lets a machine try. Turning it off stops the listener and the tunnel; paired Nodes reconnect when it is on again.

### Installing a Node remotely over tailcat

From **Devices → Add device → Install remotely**, the Hub can install a Node on another machine itself. Both machines need `tailcat`; the Hub also needs OpenSSH's `ssh` and `ssh-keygen`.

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

A local Node holds its home exclusively until its last handle is dropped after shutdown. The
lock is released explicitly, so an unrelated command between `fork` and `exec` cannot prolong
that ownership through an inherited descriptor. After an adapter exits, its stdin task ends
and its stdout and stderr drain for at most 500 ms before their tasks are cancelled and joined.
Session shutdown also waits for this transport cleanup; an unconfirmed cleanup keeps its process
record for reconciliation. This does not claim termination of unrelated processes that retain a
pipe. Runtime-wide task counts alone do not identify a Node's sampler or prove process ownership.

Runtime status in global Bot/model selectors and the model catalog are samples from the Hub's local Node. They do not certify another Node, and local unavailability does not block global configuration, projectless/automatically routed requests or proposals. Proposals create a pending decision without opening a session. When a catalog is unavailable, retain the target's default or enter its model ID. Choose permissions explicitly when support is unverified; stored configurations remain editable and Steve never automatically selects full access. Declared permission restrictions and server-side purpose restrictions still apply. The backend's existing placement, workspace ownership, Node eligibility and permission checks decide whether execution can start. Device-page component paths, discovery, installation and use shortcuts remain local to the Hub.

全局 Bot/模型选择器中的运行状态和模型目录来自 Hub 本机 Node，不代表其他 Node 已通过验证。本机不可用不阻止全局配置、无项目/自动路由请求或提案；提案只建立待确认决定，不开启会话。目录不可用时可保留目标默认或手填模型 ID。权限未验证时应明确选择，既有配置仍可编辑，不会自动选择完全放开。已声明权限合同、服务端用途限制及既有目标选择、工作区所有者、Node 资格和权限校验继续生效。设备页的组件路径、发现、安装和使用快捷入口仍只作用于 Hub 本机。

### The ACP adapters

Steve includes 17 built-in ACP coding CLI entries. Thirteen are npm packages that Steve can install and keep up to date under `<home>/adapters` (a prefix of their own, not the global one); four are installed with their own installers and discovered on `PATH`:

| Harness ID | Name | Source (`npm` package or `PATH` binary) | Adapter override | Default model override | Principal state |
|---|---|---|---|---|---|
| `claude-code` | Claude Code | `@agentclientprotocol/claude-agent-acp` | `STEVE_ADAPTER_CLAUDE` | `STEVE_DEFAULT_MODEL_CLAUDE` | `CLAUDE_CONFIG_DIR` (`~/.claude`) |
| `codex` | Codex | `@agentclientprotocol/codex-acp` | `STEVE_ADAPTER_CODEX` | `STEVE_DEFAULT_MODEL_CODEX` | `CODEX_HOME` (`~/.codex`) |
| `pi` | Pi | `@openma/pi-acp` | `STEVE_ADAPTER_PI` | `STEVE_DEFAULT_MODEL_PI` | `PI_CODING_AGENT_DIR` (`~/.pi/agent`) |
| `gemini` | Gemini CLI | `@google/gemini-cli` | `STEVE_ADAPTER_GEMINI` | `STEVE_DEFAULT_MODEL_GEMINI` | `GEMINI_CLI_HOME` (`~/.gemini`) |
| `github-copilot-cli` | GitHub Copilot CLI | `@github/copilot` | `STEVE_ADAPTER_COPILOT` | `STEVE_DEFAULT_MODEL_COPILOT` | `COPILOT_HOME` (`~/.copilot`) |
| `opencode` | OpenCode | `@opencode/cli` | `STEVE_ADAPTER_OPENCODE` | `STEVE_DEFAULT_MODEL_OPENCODE` | `XDG_*_HOME` (`~/.local/share/opencode`) |
| `qwen-code` | Qwen Code | `@qwen-code/qwen-code` | `STEVE_ADAPTER_QWEN` | `STEVE_DEFAULT_MODEL_QWEN` | `QWEN_HOME` (`~/.qwen`) |
| `goose` | goose | `@aaif/goose-acp` | `STEVE_ADAPTER_GOOSE` | `STEVE_DEFAULT_MODEL_GOOSE` | `GOOSE_PATH_ROOT` (`~/.config/goose`) |
| `kimi-code` | Kimi Code | `@moonshot-ai/kimi-code` | `STEVE_ADAPTER_KIMI` | `STEVE_DEFAULT_MODEL_KIMI` | `KIMI_CODE_HOME` (`~/.kimi-code`) |
| `grok-build` | Grok Build | `@xai-official/grok` | `STEVE_ADAPTER_GROK` | `STEVE_DEFAULT_MODEL_GROK` | `GROK_HOME` (`~/.grok`) |
| `kilo` | Kilo Code | `@kilocode/cli` | `STEVE_ADAPTER_KILO` | `STEVE_DEFAULT_MODEL_KILO` | `XDG_*_HOME` (`~/.local/share/kilo`) |
| `factory-droid` | Factory Droid | `droid` | `STEVE_ADAPTER_DROID` | `STEVE_DEFAULT_MODEL_DROID` | `FACTORY_HOME_OVERRIDE` (`~/.factory`) |
| `codebuddy-code` | CodeBuddy Code | `@tencent-ai/codebuddy-code` | `STEVE_ADAPTER_CODEBUDDY` | `STEVE_DEFAULT_MODEL_CODEBUDDY` | `CODEBUDDY_CONFIG_DIR` (`~/.codebuddy`) |
| `cursor` | Cursor | `cursor-agent` (`PATH`) | `STEVE_ADAPTER_CURSOR` | `STEVE_DEFAULT_MODEL_CURSOR` | `CURSOR_CONFIG_DIR` (`~/.cursor`) |
| `kiro` | Kiro CLI | `kiro-cli` (`PATH`) | `STEVE_ADAPTER_KIRO` | `STEVE_DEFAULT_MODEL_KIRO` | `KIRO_HOME` (`~/.kiro`) |
| `mistral-vibe` | Mistral Vibe | `vibe-acp` (`PATH`) | `STEVE_ADAPTER_VIBE` | `STEVE_DEFAULT_MODEL_VIBE` | `VIBE_HOME` (`~/.vibe`) |
| `qoder` | Qoder CLI | `qodercli` (`PATH`) | `STEVE_ADAPTER_QODER` | `STEVE_DEFAULT_MODEL_QODER` | `QODER_CONFIG_DIR` (`~/.qoder`) |

Pi uses the open-source `@openma/pi-acp` SDK bridge because it forwards ACP MCP servers into Pi; the Registry’s `pi-acp` bridge currently does not. Its configuration, skills and sessions are isolated with `PI_CODING_AGENT_DIR`; models and thinking levels come from the adapter. Pi executes native tools without approval modes, so it offers only Steve’s FullAccess mode; planning, per-command approval and automatic approval are not claimed, and thinking levels are never interpreted as permission modes. Configure or sign in to providers in Pi on the execution Node; every principal's Pi session there uses that sign-in (its `auth.json` and `models.json`, linked) and its `defaultProvider`. Installing the adapter or passing its discovery probe is not a model-turn acceptance test.

JavaScript adapters run on Node.js; the Homebrew formula depends on it. `steve setup` installs the npm-managed ones you choose into `<home>/adapters`: tick them in the wizard (CLIs already detected on `PATH` are pre-selected, falling back to `claude-code` and `codex` when no built-in CLI is detected), or pass `--adapters claude-code,codex,gemini`, `--adapters all` or `--adapters none`. `steve adapters install <id>…` adds any npm-managed entry later (with no `<id>`, it installs or updates the adapters selected for `<home>` in `selected.json` plus any already installed there, falling back to `claude-code` and `codex` only when no selection has been recorded yet). Without npm, setup goes on and says to run `steve adapters install` once Node.js is there. No version is pinned: Steve runs their latest release. An adapter you did not choose is not installed or upgraded behind your back, and the startup check does not report it as missing.

A Node asks the npm registry (as npm is configured, so a mirror works) a minute after it starts and then once a day for the adapters installed in `<home>/adapters`. With `STEVE_ADAPTER_UPDATES=auto` (the default) it installs a newer release when it has no session or job running, and the next session uses it; `check` only shows it; `off` asks nothing. **Devices → runtimes** shows the installed and latest release for each. By hand:

```sh
steve adapters status            # installed and latest
steve adapters install gemini    # add one that was not chosen at setup
steve adapters upgrade           # install the latest now
steve adapters status node       # a separate Node's (its home: ~/.steve-node)
```

Each built-in adapter resolves its executable in order: its `STEVE_ADAPTER_*` variable, `harness.json`, `<home>/adapters`, its CLI binary on `PATH`, and (for npm-managed entries) `npm root -g`. Adapters named by `STEVE_ADAPTER_*` or by `harness.json`, found on `PATH`, or installed globally are used as they are and left alone.
Adapters named by `STEVE_ADAPTER_CLAUDE` / `STEVE_ADAPTER_CODEX` / `STEVE_ADAPTER_PI` or by `harness.json`, or installed globally, are used as they are and left alone. Use the running Hub's controlled installation API for its Node. Stop a separate Node before using its operator CLI. A prefix lease excludes installs and upgrades while a managed adapter session, job or probe is using that prefix; the lease file `.prefix.lock` must not be removed.

Run `steve-hub --check-config` in the Hub service's environment before installation or an upgrade. When a selected or explicitly configured local ACP adapter is missing, or Node.js is absent for an available JavaScript adapter (`.js`/`.mjs`/`.cjs` or a `#!/usr/bin/env node` script), preflight reports a setup warning with the setting to fix; the Hub can still start so its settings and diagnostics remain usable. `steve setup` installs the chosen npm adapters into `<home>/adapters` (Node.js required); `steve adapters status` shows them and `steve adapters upgrade` installs the latest. `STEVE_ADAPTER_*` variables point at adapters installed elsewhere. For JavaScript adapters, set `STEVE_NODE` to the Node.js executable when it is absent from the service's `PATH`. A separate Node uses its own `harness.json` and installed adapters; a successful Hub preflight does not certify a remote machine's environment.

`STEVE_PRINCIPAL_STATE` defaults to `on` on the Hub. New sessions use their principal's own harness directory on the Node (history, sessions, the Bot's skills); existing sessions retain their recorded state. `STEVE_PRINCIPAL_STATE=off` is an explicit shared-state configuration for new sessions only; it does not migrate existing session state.

**Sign-in is the harness's own, on each Node.** Steve talks to the harness installed on a Node over ACP. Which model provider the harness uses and how it signs in is the harness's own business on that Node, set up by whoever installed it there: sign it in with its own command as the Node's service account (`codex login`, Claude Code's `/login` or `claude setup-token`, Pi's `/login` or `models.json`, or a provider and key helper in its own settings). Steve never configures a model provider, a base URL or an API key for a harness, and keeps no such setting. Every principal's session on the Node, the owner's and anyone else's (a schedule's service principal too), uses that one sign-in, never a copy of it: a copy that refreshed its tokens on its own would sign the original out. The adapter declares where its sign-in is (`state.login` in `harness.json`):

- `files`: its credential files. Without `store_env`, each is a symbolic link in the session's directory to the Node's own file, made even before the Node signs in, so a later sign-in is seen. The harness reads the Node's login through it and writes a refreshed one into the Node's file, so there is one login. Only for a harness that writes through a link or replaces the link's target (Codex `auth.json`: it resolves the link, replaces the target atomically in the target's directory and serializes refreshes with a lock beside it, never replacing the link itself; older releases write through the link; Pi `auth.json`, written through the link and locked at its target, and `models.json`). A credential file the harness or a person put in the session's directory is left alone (a sign-in of that directory's own; remove it to use the Node's). The Node lists the links it made in `.steve-login.json`.
- `renew`: the names the harness makes beside its credential files while it renews its sign-in (its lock and temporary file; a trailing `*` matches any rest). Only used to open them to a sandboxed session, below.
- `store_env`: a variable that moves only the harness's credential store (Claude Code `CLAUDE_SECURESTORAGE_CONFIG_DIR`, which names its macOS keychain entry and the directory of its `.credentials.json` apart from `CLAUDE_CONFIG_DIR`). The session gets the Node's own: the Node's value of it, else the directory the Node's harness uses by its `CLAUDE_CONFIG_DIR`, else empty, which is the harness's default store. Nothing is copied, and nothing is linked: Claude Code refuses a linked credentials file. Checked with Claude Code 2.1.308; a release that does not know the variable keeps its login per configuration directory and is not signed in in a session's own directory.
- `settings` and `entries`: the sign-in entries of its settings file (Codex `config.toml`: `model_provider`, `model_providers`, `forced_login_method`, `forced_chatgpt_workspace_id`, `chatgpt_base_url`, `openai_base_url`; Claude Code `settings.json`: `apiKeyHelper`, `awsAuthRefresh`, `awsCredentialExport`, `forceLoginMethod`, `forceLoginOrgUUID` and the `ANTHROPIC_*`, `CLAUDE_CODE_USE_*`, `AWS_*`, `CLOUD_ML_REGION` and `VERTEX_REGION_*` variables of its `env`; Pi `settings.json`: `defaultProvider`). In every session's settings file these entries are the Node's own as they are there when its harness opens: the ones the Node's file has come in, the ones it no longer has go, and everything else in the session's file stays the session's (what the harness wrote there itself). The file is made only when the Node's has such entries. A session of anyone but the Node's owner gets nothing else of the Node's file (no instructions, MCP servers, hooks, permissions or other settings); the owner's session starts from the whole file (the adapter's `state.import`, copied once) and gets the same entries kept current. A settings file that is a link or that the harness could not read is left alone. A provider file the model access of 0.2.114 wrote into a service session's directory (listed in `.steve-model-access.json`) is removed at its next open.

The Node owner's personal context (their user-level instructions, skills, prompts, subagents and commands) follows [its own switch](#the-owners-personal-harness-configuration), only for the owner's sessions. A service session therefore runs on the harness's sign-in on the Node but without the owner's personal instructions or other settings.

Codex keeps its login in `auth.json` unless its `cli_auth_credentials_store` is `keyring` or `auto`; it then names the keyring entry after its own directory, which another directory cannot share, so keep the default `file` on a Node whose sessions should use its login. With the OS sandbox on (`STEVE_CONFINE`), a session of anyone but the owner does not see the owner's harness homes. What renewing the Node's sign-in needs there is opened to it, so a renewal is never made and then lost (which would spend the Node's sign-in): the credential files and the lock and temporary files the adapter declares in `state.login.renew` (Codex `auth.json.lock`, `.auth-*`; Claude Code `.storage-write.lock`, `.credentials.json.tmp.*`; Pi `auth.json.lock`). Seatbelt opens exactly those names; Bubblewrap binds the store's directory and hides everything else in it again. A key helper that reads a file elsewhere in a hidden harness home is not opened, so keep such a key outside the harness homes.

When a harness on a Node is not signed in, the Node's probe cannot open its scratch session (the ACP adapters of Codex and Claude Code refuse a session without a sign-in) (`session_error` on the Devices page, with the sign-in methods the adapter offers); the harness is not ready there and the [preflight](#sign-in-and-the-preflight) refuses its Bots before anything is made, pointing at the Devices page. Sign the harness in on that device and refresh Devices.

To inspect a session's recorded principal state while the Hub is running, pause the target session and call `GET /api/sessions/<session-id>/principal-state` authenticated as the instance owner in that session's workspace (`x-steve-ws`). It returns `bound`, `node_id`, `principal`, `harness`, `independent`, `state_dir`, `harness_rel_dir` (`state/<state_dir>/harness`) and, when `node_id` is `"local"`, `local_harness_dir` (`<configured Hub data directory>/node/state/<state_dir>/harness`; `null` for a separate Node, whose directory is `<its STEVE_HOME>/node/local/<harness_rel_dir>`). If `independent` is `false` (`state_dir` is `null`), the session has no recorded independent directory. The ordinary session resume action (`GET /api/sessions/<session-id>/resume`) includes the recorded local state directory environment only for the instance owner once a local-Node session already has a native harness session ID.

`STEVE_TOOLCHAINS` defaults to `on` on each Node. Detected build outputs go into the item's `.steve/build/` and cache-only directories go into the known principal's Node cache; the first build after relocation may be cold. A session without a principal receives no invented cache or identity. Set `STEVE_TOOLCHAINS=off` on that Node to retain the original toolchain locations. Toolchain relocation and separate harness directories are not claims of complete principal isolation: Codex retains its own sandbox and approval controls, Linux outer confinement is on by default where bubblewrap is installed, and the existing remote Node single-owner rule and workflow same-principal admission still apply.

### Your own contact profile

Open your own contact page to edit identity, work, contact details and preferences. Every attribute shows its source. Inferred attributes stay candidates until you confirm them; rejecting one hides it from the active facts. Original notes remain unchanged and can be expanded below the structured facts. The knowledge steward receives this profile with its memory list and can propose personal memories using `steve_contacts_remember`; those proposals still need confirmation, and do not automatically become structured attributes.

A missing email is supplemented from the signed-in Hub user's record, not from the Hub host's git configuration or locale. Existing email attributes, including stale ones, and recorded deletions suppress supplementation. Your manual edits and confirmations are retained. Other account and organization details come from their owning connected modules; no machine-local profile source is assumed.

**Steve calls you** (`call`) is for internal conversation; **Introduce you as** (`call_external`) is for external drafts, introductions and handoffs. Neither falls back to the other: a missing value uses the contact name. Optional `call_ext_scope` attributes have the value `{"kind":"persona"|"space","id":"<Hub ID>","name":"<external name>"}`. Select an available persona or active conversation in the page rather than entering a platform ID. A conversation override takes precedence over a persona override, then the general external name. Only confirmed facts whose disclosure level permits the use participate; candidates and `never` facts do not, and reasoning-only names are not used externally. These facts set a name, not access to that conversation or account.

Default persona introductions do not store a person's name. Untouched historical default templates are returned without the captured name; custom introductions and introductions with an edit history are preserved, and stored text is not rewritten. A draft sent as you uses the current connection's verified active account linked to a current workspace member, not the workspace owner or the connection creator. A removed or unverified account identity cannot borrow someone else's profile. In the personal page, a mail account matching a confirmed email fact is linked beneath that fact instead of repeating the address; distinct accounts remain visible.
### Pi installation and sessions

Pi is built-in id `pi`. It needs two MIT-licensed npm releases: the official [`@earendil-works/pi-coding-agent`](https://www.npmjs.com/package/@earendil-works/pi-coding-agent) and [`@openma/pi-acp`](https://www.npmjs.com/package/@openma/pi-acp). The latter runs Pi in-process using the official SDK and registers HTTP MCP servers and their headers with Pi’s native MCP extension. The upstream package named `pi-acp` does not support HTTP MCP and cannot provide Steve's tool integration. Pi SDK must be at least 1.1.0 for that forwarding; the current official release requires Node.js ≥22.19.0. npm installs with engine checks enabled. Future package engine requirements may be higher.

1. Install a compatible Node.js and npm on the **execution Node**. If the service does not find Node.js, set `STEVE_NODE` to its executable and put its directory on the service's `PATH` (npm's launch scripts also need it). No compiler, source checkout, global npm prefix or publishing credentials are needed.
2. As the instance owner, read `GET /api/devices?fresh=1`. The Pi row's optional `setup` describes `components` (`id`, `name`, `package`, `installed`, `version`, `path`), `can_install`, `reason`, `help` and `error`. `not_installed` includes partial installations; `installing` means accepted work is still running; `unavailable` means a prerequisite, install or real ACP session probe failed; `unknown` means the Node cannot currently establish the result. `ready` requires a successful session probe. The row's `ok` controls session availability. File presence and ACP initialization alone do not establish readiness. A disconnected or unprobed Node reports unknown, with installation disabled. A forced refresh that finds it disconnected also clears prior readiness from `/api/state` and the session/model catalogue. Older in-flight model probes cannot restore readiness or models after a newer disconnect, failed probe or unavailable result has been published; reconnect and refresh to restore availability. An older Node's successful ACP session probe remains usable even when it cannot report `setup` or offer controlled installation.
3. Send `POST /api/system/harnesses/pi/install` with an empty body (or `{}`). It accepts no package, version, shell command or destination. The Node asynchronously installs both registry-declared packages into `<STEVE_HOME>/adapters`, never globally, and returns `{"accepted":true,"restart":false}`. Poll `GET /api/devices?fresh=1` until setup finishes. Repeated requests join the existing operation, including a completed successful install whose component entry points are still present. Use the updater or operator CLI to explicitly upgrade an existing complete installation. Installation and daily updates share the same admission and prefix leases with running harnesses and jobs; a conflict returns HTTP 409. Release idle sessions and wait for active work before retrying. npm may take several minutes; an HTTP disconnect does not cancel it.
4. Configure a model provider using Pi's own [provider instructions](https://github.com/earendil-works/pi/tree/main/packages/coding-agent/docs) on the **execution Node**, in Pi's own directory there (`PI_CODING_AGENT_DIR`, default `~/.pi/agent`, with `settings.json`, `models.json`, `auth.json` and `skills/`). Steve supplies no credentials and does not log in on your behalf. Sessions use `<Node home>/state/<recorded state_dir>/harness` with principal state enabled; there, `auth.json` and `models.json` are links to the Node's own and `defaultProvider` comes from its `settings.json` ([sign-in](#harness-setup)), so every principal uses the Node's Pi sign-in. Provider configuration is an operator step; refresh discovery after it is complete.
5. Choose Pi and one of its probed `provider/model` ids for a bot, using **full access** (`bypassPermissions` in the bot API, `full_access` in the Node contract). Start an item/session with that bot. Models, thinking efforts, image support and session loading come from the adapter's actual response. Installation invalidates the Node probe cache; refreshing Devices also updates the Hub's model/session catalogue immediately, without restarting the Hub.

Pi's ACP modes are thinking levels. Steve never maps `off`, `low`, `high`, etc. to permission grants. This bridge supports only `full_access`; Ask, Plan, accept-edits and Auto requests are explicitly refused, including requests made outside the UI. Thinking effort is set through ACP `thought_level`. Pi's built-in file/shell tools do not use Steve's approval cards or YOLO's per-request decisions. The Node's configured OS confinement still applies where available. This adapter does not expose Steve's custom system-prompt or tools-off configuration; use it for trusted full-access sessions, and keep read-only research or restricted model jobs on an adapter that can enforce those policies. HTTP MCP forwarding is supported; SSE MCP and importing Pi's native history into Steve are not implemented.

The SDK bridge uses Pi’s native session store in `PI_CODING_AGENT_DIR`; it does not spawn a second Pi CLI or require a separate bridge HOME/session map. Legacy external bridges may still declare an auxiliary `state.home`. Each principal's Pi state stays separate; its sign-in is the Node's, as described above.

For a failed install, correct Node/npm, registry/network, disk or package engine problems, then explicitly retry the same endpoint. A Node restart during installation preserves `unknown` in its journal and never automatically resumes it. A new request is allowed only when the old installer is confirmed gone and the prefix is free. If the Node itself is offline, reconnect it before inferring whether installation completed. Setup errors stay visible; a successful install can still be unavailable until the model provider is configured. With the Hub stopped, `steve adapters install pi` is the equivalent operator command; on a separate Node use its own installation home and CLI. Daily updates inspect both Pi packages after an explicit successful installation, and never resume a failed or interrupted setup for you.

For an existing external install, set `STEVE_ADAPTER_PI` to the bridge's `dist/bin.js`, or declare `pi` in the Node's `harness.json`. The managed bridge uses its Pi SDK dependency in-process; an existing Pi CLI on PATH is discovery evidence, not a substitute for missing managed SDK files. A globally installed bridge discovers its Pi CLI on the Node service's `PATH`; its packages remain operator-managed. A missing component in a managed installation is still reported missing even when a global CLI exists. Such overrides disable controlled installation and automatic updates for that entry. A custom bridge can declare `permission_policy` (`acp` or `full_access_only`), `require_mcp_http`, `state` and `skills` as properties. Existing adapters keep their default ACP permission mapping. General user environment settings cannot replace `PI_*` harness configuration.

The optional component fields describe evidence separately from runtime availability. `status` is `installed` (entry and required package contents verified), `missing` (required entry/content is absent), or `unknown` (integrity cannot be verified, such as an external executable wrapper without npm metadata). When `status` is absent, older Nodes' `installed` boolean remains the compatibility fallback. `detected: {path, version?}` reports another executable found on the Node's PATH; it does not change the component's `path`, its installation target, or the session command. For example, PATH-only Pi is reported as another installation while the missing managed bridge and managed Pi target remain explicit. Steve does not silently repair a broken managed installation with the global Pi.

For external executables, a successful real ACP `initialize` and `session/new` probe is evidence that the command works even when `STEVE_NODE` or npm is missing. Controlled installation remains disabled. A JavaScript entry that actually uses `STEVE_NODE` still needs that interpreter, and a managed installation still checks its package engine requirements. Component integrity can remain unknown while the runtime is ready; unknown is not software missing. Old Nodes without setup fields retain their successful session evidence.

Accepting installation invalidates prior runtime and model discovery, including pending explicit/background probes. A probe started before acceptance cannot restore an old ready snapshot afterward; the next Devices/state read obtains the Node's current installing state. The state response cache also checks the runtime generation, so its short TTL cannot resurrect pre-install readiness.

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

The Hub is the only entry point for tools. A session's Node and harness receive Steve's endpoint only, not plugin server configurations or credentials. The Hub proxies the servers the Bot has switched on and shares one connection per library server across its sessions and Bots. `stdio` commands run on the **Hub host**, through the Node's OS sandbox API with a private temporary working directory; install the command and its dependencies on that host, not on each session's Node. HTTP and legacy SSE endpoints are reached from the Hub. Confinement follows `STEVE_CONFINE`, including its documented unavailable/off behavior; an MCP server is trusted operator-installed code, not a new permission boundary. Servers needing a project directory must receive an explicit configured path/argument; there is no per-session working directory or per-session plugin process.

Secret values go into the workspace's system vault (one credential item per server, fields `env:<name>` and `header:<name>`). Rows and API answers retain references, not secret values. The Hub resolves credentials when it opens the shared connection and records the use against the server in the vault use log. Opening a harness does not take plugin credentials. Saving a server without a secret's value keeps the stored one; deleting it destroys its credential item. A missing credential or unavailable vault makes that server unreachable, not a blank credential substitution. A plugin can itself return sensitive content; this proxy does not mask tool inputs or outputs.

Catalogs refresh in the background after ten minutes; failed first listings retry after one minute. Edits invalidate the old revision, and the minute sweep closes deleted/changed connections and connections idle for ten minutes. A cold pinned listing waits up to 20 seconds; search/discovery up to 45 seconds while the shared listing continues in the background (60-second listing budget). Calls have a 115-second overall budget including discovery. An unreachable server is reported in search results, while other sources remain usable. A timed-out call is **not retried**; its outcome may be unknown, so inspect the remote system before repeating a write. HTTP redirects are not followed, and legacy SSE message endpoints must stay on the same origin as the stream.

#### Tool discovery and Bot pins

Each session lists these eight core tools, followed by available Bot pins: `steve_update`, `steve_ask`, `steve_request_approval`, `steve_deliver`, `steve_items_search`, `steve_knowledge_search`, `steve_tools_search`, `steve_tool_call`. Other built-in tools, enabled MCP tools and enabled A2A agents remain callable without loading all their schemas into every turn. A short, bounded catalog summary in the session brief names capability areas and example tools. There is no dependency on MCP `tools/list_changed`: a pin is reflected when a harness next lists its tools; generic search/call works without a harness-specific discovery feature.

- `steve_tools_search {"query":"comment on a pull request","source":"mcp","kind":"write","resource":"repository","limit":8}` uses deterministic keyword matching, not a model. All fields are optional; `source` accepts `steve`, `mcp`, `a2a`, `connector` or a source name, `kind` is `read|write`, and `limit` is 1–25 (default 8). Empty queries plus filters browse a source. Already-listed tools are omitted except an exact tool-name query. Results include definitions, `more` when matches exceed the limit, and unreachable-source diagnostics; narrow the query to see other matches.
- `steve_tool_call {"name":"<returned name>","arguments":{...}}` validates the target's JSON Schema before dispatch, including local `$ref` and composition. Invalid arguments return an error without invocation. HTTP/file schema-reference retrieval is disabled; schemas requiring external references fail validation. The wrapper cannot call itself or the search wrapper. Pins change visibility, never capability authorization; disabled or unassigned sources cannot be called through a cached definition.
- Definitions use MCP `name`, `description`, `inputSchema` and `annotations`. MCP server names are namespaced as `<server>__<tool>`, capped at 64 ASCII characters with a stable hash for long or sanitized names. Existing MCP annotations are preserved. `annotations.steve` supplies `source` (`steve`, `mcp:<name>`, `a2a:<name>`), `kind`, `resources` and `asYou`. Plugins may declare `resources: [string]` and `asYou: boolean` under their own `annotations.steve`; defaults are the server name and false. Read/write uses `readOnlyHint` (missing means write). These are descriptive hints, not authorization or an implemented output-pipeline enforcement policy. Connector operation declarations are not part of this release.
- Plugin results currently cross the Steve bridge as text: text content is retained, non-text content is described, and `structuredContent` is serialized when no text is present. Rich images/audio/resources are not passed through as native MCP blocks. Oversized results are truncated with an explicit note. A catalog must contain named tools with object `inputSchema`; malformed/duplicate tools or more than 20 catalog pages are reported as listing errors rather than silently weakened or truncated.

In a Bot's **Capabilities → Tool loading**, expand **Choose tools to load directly** to search the Hub catalog, assigned MCP/A2A sources and tools in that Bot's usage history. Pin or unpin an individual tool or an enabled provider; core tools are marked always loaded. The page shows the 32-pin limit and 30-day search/call counts, reports save failures without pretending a change succeeded, and keeps unavailable existing pins removable. This is a known-tool catalog, not a live listing of every remote tool; agents still discover tools in their actual session, under its current account and permissions. Pinning never grants access.

Pins and usage are also available through the authenticated workspace API:

- `GET /api/bots/{id}/tools` → `{core, pins: [{kind,target}], usage: [{tool,searched,called}], days:30}`.
- `PUT /api/bots/{id}/tool-pins/{kind}/{target}` pins; `DELETE` the same path unpins. `kind=tool` names one tool (including an MCP tool); `kind=mcp` names a server ID and pins all its tools; `kind=a2a` names an agent ID. MCP/A2A entries must be enabled for that Bot when pinned. Up to 32 pins per Bot, duplicate requests are idempotent, core tools need no pin. Unassigning/deleting an entry removes its server/agent pin; unavailable tool-name pins are simply not listed. Changes are audited as `bot_tool.pin|unpin` and emit a Bot change event.
- Usage is per Bot and tool: `searched` counts appearances in returned search results, `called` counts dispatched attempts (including remote errors, excluding failed argument validation). Updates are asynchronous, grouped by UTC day, summed over 30 days and retained for 90 days. This is not billing or an exactly-once execution ledger.

#### Diagnose model tool failures

Adapter discovery runs `initialize` and a scratch `session/new` without a model prompt. Its transport capabilities and model list describe what the adapter offers; they do not certify each model's tool support. A model accepted by `session/set_config_option` can still fail when asked to call a tool. Keep the adapter's reported model id, selected configuration, version and original error when diagnosing this.

Verify three stages separately in a bounded diagnostic session with its [own harness state](#its-harnesses-own-state): session startup, an exact text reply with no tools, and a read-only MCP call. For the last stage, require a model-initiated `tools/call`, the service's later response with the same request id, a successful ACP completion linked to that tool's call id and returned marker, and a reply after the last tool event containing the marker known only to the service. A native file read, an MCP `initialize` / `tools/list` exchange, an unrelated completed tool, or a model saying that it called a tool does not establish that round trip. Keep partial ACP and MCP events on timeout; do not record credential values, authorization headers or complete harness state in diagnostic artifacts.

For a maintainer working from a checkout, `python3 scripts/diagnostics/probe-codex-mcp.py --root <private-state-root> --output <evidence-root> --source-config <owner-config.toml> --adapter <installed-codex-acp-entry.js> --label <unique-run> --model <offered-model-id>` runs this diagnostic against an installed Codex adapter. It uses fresh state, the owner's provider settings and auth helper, read-only mode and `high` effort; it does not copy logins, hooks, existing MCP servers or conversation state. The state root and evidence root must be separate. Only the evidence root is for sharing; keep the state root and its stderr private. `--trace-provider` optionally forwards Responses requests through a local loopback observer and records tool schemas and calls, without request headers or prompts. Use only the generated diagnostic traffic; the observer's tool-result records are not a general-purpose redaction filter. No running Hub is started or contacted, and a failed stage exits nonzero. Run its offline checks with `python3 scripts/test-acp-mcp-probe.py`; ordinary installations do not need this source tool to operate.

On macOS and Linux the probe samples adapter descendants during the run and before processing ACP responses, recording their PID, parent and native process birth identity (Darwin `proc_pidinfo` or Linux `/proc`). Cleanup retains tracked descendants even after their parent exits. It verifies identity immediately before signalling each PID, waits up to two seconds after SIGTERM, sends SIGKILL only to verified survivors, then waits and reads back every tracked process. Linux uses pidfds for signalling when available. `result.json` includes per-PID signal, wait and final-state receipts; absent or zombie processes are terminal, while a surviving process, unreadable/changed identity or discovery failure makes cleanup and the diagnostic fail even when every protocol stage passed. It never signals by name or process group. Discovery is sampled: a descendant that disappears from the ancestry before it was ever observed cannot be safely claimed or cleaned by a guessed PID. The receipt covers observed descendants, not every process on the machine. Review a failed cleanup receipt before retrying; never reuse a historical PID for recovery.

Compare a failing model with a known working model using the same adapter and fixture. Steve passes `mcpServers` through ACP `session/new` / `session/load`; the adapter and its harness translate model tool calls. A harness router's `unsupported call` is evidence of a rejected call, not proof that Steve's service received it. Check the actual tool names and protocol at the failing boundary before changing configuration or attributing a fault. A local MCP fixture confirms only that diagnostic path; it does not verify the running Hub's authentication, tools or original task recovery.

Keep original health failures and setup errors in their evidence files when reviewing diagnostics. Record code regression checks and expected negative cases separately: a correctly retained failure is evidence that the diagnostic works, not that the failing model is healthy. Replaying saved protocol events does not constitute a new live model test. A configuration file drafted offline has no native changeset registration, apply or undo receipt; those and a new Steve session's real tool round trip remain separate recovery steps.

For a reversible model substitution, record the Bot id, revision and current `harness`, `model` and `reasoning_effort` from `GET /api/state` (`bots`). Choose an exact model id the adapter offers, and patch only the intended fields through `PATCH /api/bots/{id}`. A harness id and a model id are separate fields; do not construct a model id by joining them. Bot configuration is copied when a new session is created; changing the Bot does not switch an existing session's model. Verify the effective model and all three stages in a new test session before declaring recovery or continuing business work.

The Bot PATCH endpoint currently has no revision precondition or `If-Match` support. A read followed by a PATCH is not an atomic comparison: do not automate apply or undo while another writer can change the Bot. Serialize configuration writers first, re-read and refuse on any revision or before-value mismatch, then retain the response and re-read the resulting revision and fields. Undo must similarly match the recorded post-apply revision and values and restore only the changed fields; if serialization cannot be established or the Bot has changed, stop and refresh the draft instead of overwriting it. Do not send an invented revision field or rely on an ignored HTTP precondition.

#### Skills

A skill is a `SKILL.md`: a `name` (lower-case letters, digits and single hyphens, at most 64, unique per workspace), a one-line `description` of when to use it (at most 1024 characters) and Markdown `content` (at most 32 KiB). A bot's switched-on skills may hold at most 128 KiB together; switching one on, or growing a skill some bot has on, past that is refused. The page can fill a new skill from a pasted `SKILL.md`; front matter other than `name` and `description` is not kept.

Whether a harness reads skills, and where, is declared by its adapter (the `skills` property: a directory under the harness's own state directory). The built-in Claude Code and Codex adapters declare `skills`, so a bot's skills go to `$CLAUDE_CONFIG_DIR/skills/<name>/SKILL.md` and `$CODEX_HOME/skills/<name>/SKILL.md`, where each harness reads its user skills. An adapter in the harness manifest declares its own (`"properties": {"skills": "skills"}`) or none; for one without it, the page says the harness does not read skills and the skills are kept unused.

When a session of a bot opens (or reopens), the Hub puts the skills the bot has switched on into the Node's `open_session` command, and the Node writes them into the session's own harness directory (`state/<state_dir>/harness`, see the principal harness state above) before ACP `session/new` / `session/load`. The Node records the skills it wrote in `.steve-skills.json` beside them and keeps that set exactly the session's: a changed skill is rewritten, a removed or switched-off one loses its `SKILL.md` (its directory goes only when nothing else is in it), and a skill the harness or a person put there is left alone. A change applies the next time a session's harness opens. A session without a harness directory of its own (`STEVE_PRINCIPAL_STATE=off`, or an adapter that cannot move its state) gets no skills; the Node logs that. An older Node ignores them.

#### The owner's personal harness configuration

A harness run by Steve keeps its state in the session's own directory, not in the owner's `~/.claude` or `~/.codex`, so by itself it would not see the owner's personal, user-level configuration there. For a session of the Node's owner, the Node links that configuration into the session's directory each time its harness opens, so the harness finds it where it would without Steve: the adapter's `state.context` entries, relative to the owner's harness directory. Built in: Claude Code `CLAUDE.md`, `skills/*`, `agents`, `commands`, `output-styles`; Codex `AGENTS.md`, `AGENTS.override.md`, `skills/*`, `prompts`; Pi `AGENTS.md`, `skills/*`, `prompts`; Gemini CLI `GEMINI.md`, `commands`; Qwen Code `QWEN.md`, `commands`. An entry ending in `/*` links each child on its own (names starting with `.` are the harness's own and are skipped). They are links, not copies: an edit the owner makes is what the next session reads, and nothing is written through them. The Node lists the links it made in `.steve-context.json` and keeps them the owner's current set; a file or directory the harness or a person made there is left alone, and a bot skill of the same name gives way to the owner's own. A session of anyone else gets none of it. The harness's sign-in is not personal context: every principal's session on the Node uses it ([Harness setup](#harness-setup)). A custom adapter declares its own `context` inside `state` in `harness.json` (`[]` for none).

The project's own instructions and skills (`AGENTS.md`, `CLAUDE.md`, `.agents/skills`, `.claude/skills` and the like in the working directory) are read by the harness from the worktree as usual and do not depend on this.

Whether the owner's personal configuration is loaded is a choice: **Devices → Hub → Defaults for every Node → Personal agent config** sets the Hub's default (`GET`/`PUT /api/node-context` with `{"owner_context": "on" | "off"}`, instance owner only; `on` by default, stored as `settings` row `node.owner_context` of workspace `default`), and **Devices → Nodes → a Node → Personal agent config** follows it, or turns it on or off for that Node (`POST /api/nodes/{id}/context` with `{"owner_context": "on" | "off" | "inherit"}`, stored as `nodes.owner_context`). The default belongs to the Hub, not to a workspace, because a Node serves every workspace of its Hub; upgrading from a release that kept it per workspace (`ownerContext`) starts the Hub's default off if any workspace had turned it off. Turn it off to make every Node behave the same: agents then load only the project's files and what Steve adds. A change applies to sessions whose harness opens afterwards; running ones keep what they started with, and an off session loses the links at its next open. An older Hub that does not send the choice means on.

Off removes only what Steve links. A harness still reads what it finds from the working directory on its own: Claude Code also reads the `CLAUDE.md` and `.claude/CLAUDE.md` of every directory above the working directory, so for a worktree under the owner's home (Steve's default `~/.steve/worktrees`) it reads `~/.claude/CLAUDE.md` that way whether the switch is on or off. Codex stops at the repository root. To keep that file out as well, keep the Node's worktrees outside the owner's home.

#### Sign-in and the preflight

Work nobody in particular asked for (a schedule without a requester, a flow, Steve's own work in a shared workspace) runs as the workspace's **service principal** (`svc:<workspace>`). Like every principal's, its sessions use the harness's own sign-in on the Node they run on ([Harness setup](#harness-setup)); there is nothing to configure at the Hub for them, and Steve never asks for a provider address or key. They do not get the Node owner's personal instructions, skills or other settings.

**Preflight.** Before anything is made, the Hub checks deterministically whether a principal's Bot can run at all (`can_run(principal, bot, harness)`): the Bot's harness is not reported missing, not ready or not signed in by every Node that could take it. A Node that is linked but has not answered yet leaves its harnesses unknown, never refused. It reads only the database and what the Nodes last reported; it decrypts nothing and asks no model. It runs:

- when a schedule is created, edited or enabled (an enabled `item`, `item_auto` or `propose` schedule; reports make no session), and when an item is given to another Bot: refused with the reason and where it is fixed;
- when a schedule fires, before its item is created: no item is made, the run is recorded as failed with that reason, and nothing is tried again by itself;
- before a session is opened for an item: no session, one fail card on the item (the same trouble is not asked twice), no retry loop.

The reason points at the Devices page: install the harness or sign it in on the device. The schedules page shows it on each schedule that cannot run now.

**Whose access a schedule uses.** A schedule runs as its workspace's service principal unless its creator explicitly names themselves as `requester` (personal execution, below in [Conversation digest base](#set-up-the-ordinary-digest-bot)); creating a schedule alone never makes it run as its creator. Owner-created schedules that act only on the owner's own data (a memory tidy, a personal briefing) are not switched to the owner automatically: that would hand work to a personal account without the owner saying so. Set `requester` to your own user id for those. Either way the session uses the harness's sign-in on its Node; what changes is whose data and personal context it may use.

#### Remote agents (A2A)

A remote agent is added by the address of its [A2A](https://a2a-protocol.org) Agent Card. An address that names only a host is looked up at the standard `/.well-known/agent-card.json`, then the older `/.well-known/agent.json`. The Hub reads the card (at most 512 KiB; it needs a `name` and the agent's `url`) and the page shows its name, description, version and skills before it is kept. **Read the Agent Card again** fetches it anew. If the agent wants a key, choose `bearer` (sent as `Authorization: Bearer <key>`) or a header of your own (`<header>: <key>`); the key goes into the vault (field `auth` of the agent's credential item) and is shown only as `has_secret`.

A bot that uses a remote agent gets one Steve tool for it, `steve_a2a_<name>` (the name defaults to one made from the card's, lower-case letters, digits and `_`). Its description is the card's description and skills. It takes `message`, and optionally `context_id` and `task_id` from an earlier answer: the Hub sends A2A `message/send` (JSON-RPC, blocking) to the agent's `url`, asks again with `tasks/get` while the task is `submitted` or `working`, and answers with what the agent said, its artifacts, and its task and context ids. A task still running after 100 seconds (a Node gives a Steve tool call two minutes) is answered with its `task_id`; calling the tool again with only that id gets the result later. When the agent waits for input (`input-required`), the answer says so; the bot replies with the same tool and `task_id`. The tool is Steve's, so every harness gets it the same way; it is discoverable by search, and appears directly in the next tools list when pinned. Each use of the key is in the vault's use log. Streaming and push notifications are not used.

#### Steve as an agent

Steve publishes its own Agent Card at `/.well-known/agent-card.json` (and `/.well-known/agent.json`) on the Hub's address, without sign-in. Its `url` is `<the Hub's address>/a2a`, the address taken from the configuration (`STEVE_PUBLIC_URL`, `STEVE_PUBLIC_URLS`, else the local network's, else the one the request came by). **Capabilities · A2A** shows both addresses. `POST /a2a` takes JSON-RPC 2.0 with one of the user's access tokens (`Authorization: Bearer stt_…`; `x-steve-ws` names a workspace when the user has several):

- `message/send`: a message without `taskId` becomes a new item the user asked for (source `A2A · <token name>`), its bot and title chosen as for a message; with the `taskId` of one of the user's items it adds the text to that item. The answer is the task at once; its `id` and `contextId` are the item's id.
- `tasks/get {"id"}`: the item's state (`submitted` before its first session, then `working`; `input-required` while a question, approval or acceptance is open for the user; `completed`, `canceled` or `failed` when it ended), what Steve reads of it as the status message, and its last delivery as an artifact once it is done.
- `tasks/cancel {"id"}`: drops the item.
- `message/stream`, `tasks/resubscribe` and push notification settings are answered as not supported.

#### Steve's MCP for clients outside Steve

Steve's own tools are an MCP server too: every bot's session has it ("Steve · built-in" on the Capabilities pages, always on, its tools folded inside with what each is for). A client outside Steve – Claude Desktop, Cursor, another agent – can use the same endpoint, `/mcp` on the Hub's port, with one of its user's access tokens (see [Access tokens and clients](#access-tokens-and-clients)) sent as `Authorization: Bearer stt_…`. Unlike a Steve session's own token, which works only from the Hub's machine, an access token is accepted from any address the Hub answers. The client acts as that user in their first workspace; `?ws=<id>` on the URL or an `x-steve-ws` header names another. A revoked or expired token is refused at once.

Such a client lists `steve_knowledge_search`, `steve_items_search`, `steve_item_create`, `steve_tools_search` and `steve_tool_call` by default. Search/call can discover and run only the following session-independent allowlist, never a Bot's plugin catalog:

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

Cursor takes the same without `type` in `~/.cursor/mcp.json`. Claude Desktop starts local commands from `claude_desktop_config.json`, so it reaches the Hub through `mcp-remote` (Node.js): `{"command": "npx", "args": ["-y", "mcp-remote", "https://<hub>/mcp", "--header", "Authorization:${STEVE_AUTH}"], "env": {"STEVE_AUTH": "Bearer stt_…"}}`. Claude Code: `claude mcp add --transport http steve https://<hub>/mcp --header "Authorization: Bearer stt_…"`. A client on another machine needs the Hub to listen beyond loopback, or a public address with HTTPS from your own reverse proxy (see the setup above). `GET /api/capabilities/steve` answers the built-in definitions (with schemas, annotations and `core` flags), default core names and addresses: `{tools, core, outside, outside_core, endpoints: [{url, kind}], agent_card, a2a}`. `outside` describes the full outside-client allowlist; `outside_core` names its three base tools, with search/call added to its default MCP list.

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
`localStorage` under `steve-appearance` (light/dark under `steve-theme`); nothing of it is stored on the Hub, so the
"tell Steve" box on that page, which drafts Hub settings, does not change it. Font size and Reduce motion on the same
page are Hub settings and follow you to every browser.

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

A [theme plugin](#plugins) installed on the Hub is registered this way for everyone on it. Themes from elsewhere are added in the web client with `registerTheme(manifest)` from
`web/src/themes` (also `window.steveThemes.registerTheme` for a script that is not bundled with the client). It
returns `{ ok: true }` or `{ ok: false, errors }`: a manifest with an unknown field or token, a token its kind may not
set, or a value outside the syntax above is refused as a whole and nothing on screen changes. A registered theme
appears among the choices of its kind; a stored choice whose theme is not registered shows the default until it is.


### Hub management and personal aliases

Open **Devices → Hub** (`#/devices/hub`; the old `#/settings/hub` leads there) from the Hub
menu's **Devices & connections**, or from **Me → Devices & connections** on a phone. The page
manages the connected Hub; switching connections is a separate action. Its shared name and
purpose note are visible to signed-in workspace members, and only the instance owner
(`users.role=owner`, not a workspace administrator) can edit them. Version, address and stable
identity are read-only. The same page shows the computer the Hub runs on (CPU load as the
1-minute load average over the cores, memory, the fullest project disk) with the way into the
Hub's own Node, how remote Nodes reach the Hub (whether it listens, tailcat, the default
upgrade policy) and how clients sign in. The Hub's own Node is the inventory's unique explicit
`node_id=local` association; names and Hub roles never establish identity.

`GET /auth/hello` declares `capabilities.hub_metadata: 1`. Without this declaration the
editor reports unsupported, while existing connection and pairing features remain usable;
clients must not infer support from a software version or API generation.
Authenticated `GET /api/hub-metadata` returns `contract: 1`,
`capabilities.shared_metadata: 1`, `metadata`, `can_edit`, `edit_reason`, and `device_id`.
With `capabilities.addresses: 1` it also lists `addresses`, where other devices reach this Hub, the
same ones a pairing offers: `{url, page, kind}`, `kind` being `public`, `lan`, `tailnet` (a tailnet
address or `.ts.net` name) or, last, `loopback` (this machine) when the Hub listens there; `page` is
where the web client is at that address. `PUT` answers with them too.
The latter can be null; the UI can resolve the trusted inventory association described above.
Metadata contains `hub_id`, `display_name`, `purpose_note`, `revision`, `updated_at` and
`updated_by`. Audit times are Unix milliseconds and the editor is a stable user ID. Initial
values are name `Hub`, empty note, revision 0 and null audit fields. Identity reuses the
installation's persisted `node.hub_id`, independently of Node/listener availability, URL,
client directory ID, device name or the protocol's `Hello.name` value `steve`.

`PUT /api/hub-metadata` accepts only `revision`, `display_name` and `purpose_note`.
The name is trimmed, required, single-line and 1–64 Unicode code points without control
characters. The note can be cleared, is at most 500 code points after CRLF/CR normalization
to LF, permits LF/tab, and renders as plain text. Invalid input returns 400 without silent
truncation. Owner permission is re-read in the same SQLite transaction as the revision
comparison and write: revoked permission returns 403 and a stale revision returns 409.
Successful writes return canonical values, audit fields and a new revision. This revision
is independent of the Hub software version; there is no force-overwrite operation.
Shared metadata uses the existing settings table's reserved instance entry
`default / hub.metadata.v1`; backups must retain the database, including `node.hub_id`.
The device configuration revision is added by its own database migration; shared metadata needs no additional
schema migration. Normal installations apply the bundled migrations on startup.

Cancel discards a changed draft only after confirmation. Saving blocks duplicate submissions
and navigation. Errors retain the draft; uncertain results and timeouts require a read-back
and comparison before retrying, even if the read-back matches. A conflict shows the latest
values and the local draft separately; loading latest does not silently overwrite someone
else's edit. Revoked permission disables saving and keeps the draft copyable. Offline
snapshots are per-tab `sessionStorage`, keyed by the browser connection or Apple profile,
and show the absolute read time. Cached permissions do not authorize writes. With no
snapshot the page reports that nothing has been read. There is no offline write queue or
whole-site offline cache; the web client must still be available to open the page.

The separate personal alias is optional, at most 64 Unicode code points, and can be cleared.
It belongs to this browser's `localStorage` directory or this Apple client's profile store;
it is not account synchronization. Rename/clear preserves the active connection, address,
credentials and known version. Browser changes notify sibling tabs. Apple Bridge v2
advertises the optional `hubs.rename` operation with `{id, alias}` and returns `alias` from
`hubs.list` (empty string means cleared); old bridges without this capability disable alias
editing. Existing client labels are not migrated into shared names. The connection menu
can show “alias · shared name” and retains the address; the management title uses the
shared name. Neither alias editing nor shared metadata requires a private service or model
backend.

### Device work and monitoring

The device page groups visible sessions by their recorded device identity. Open requests take
precedence over execution state; a running session with a request is counted once under “Needs
you”. Read-only imports do not count as execution load. Queued messages retain their own session
and message identities; unassigned queues are not attributed to the local machine. “Status
updated” is not proof of execution activity. A disconnected device retains its last known work;
Steve does not automatically move that work elsewhere.

The built-in `hub` inventory identity is registered by the installation and is associated with
`local` only while the local Node coordinator is configured. Names and roles do not establish
this association. Remote inventory association, exact occupancy and workspace eligibility remain
unknown where the backend has no supporting contract. Local harness discovery is displayed only
for this explicitly associated local device.

When the backend reports `running` and `waiting`, the page preserves those Node-wide command
ledger counts separately from the visible workspace's work groups. Occupancy includes accepted,
active and possibly sent operations; it is not CPU activity or a guessed free-slot count.
A session's `room` evidence marks a capacity wait or reports the Node's actual refusal detail.
It remains current work without inflating the executing count. Missing room data remains unknown.

Only the instance owner can change supported local device configuration. The page refreshes
connection and permission facts every ten seconds; failed refreshes retain the snapshot and
disable configuration. The backend rejects writes while the supported Node is disconnected. Device reads include
`revision`, `node_id`, `connected`, `can_configure`, and a reason when configuration is unavailable. A
`PATCH /api/devices/hub` includes the read `revision` alongside `status` (`正常` or `暂停接活`)
and/or `max_sessions` (1–256). The response returns the committed `device` and incremented
revision. The owner role, revision comparison and write are checked in one transaction; a
revoked owner receives 403 without changing any field or revision. A stale revision returns 409.
A missing, negative or non-integer revision returns 400 (`device_revision_required_refresh_or_upgrade`);
refresh the page or upgrade an older client to load and submit the current revision. Concurrency
must be an integer from 1 through 256 in the page, API and Store. Existing values such as 32 are
preserved when pausing or resuming; unsupported values are rejected, never silently clamped.
These settings gate new logical
session turns on the local Node, including queued follow-up turns. They do not stop existing
attempts or their retries, pause orchestration, control independent background jobs, or migrate
work. A manual pause gates new logical turns before queue claiming; it does not cancel logical
turns already accepted by the Hub, including attempts already waiting in the Node command queue.
Paused, unclaimed turns are reconsidered every two seconds and when the runtime next plans work.
Capacity itself uses the canonical command-lease limiter, not a second count of session
reservations, so waiting-room evidence and scheduling remain consistent.
Lowering concurrency never interrupts an accepted attempt. The editor preserves drafts after
errors and requires a read-back before retrying an uncertain save or conflict.

Node vitals are sampled every 60 seconds with a 15-second response timeout. Samples older than
135 seconds (two intervals plus the timeout) are stale. Each result carries this policy, the
Node's sample time, the Hub receipt time, and gaps between received samples. Missing samples
remain gaps. Load history is the one-minute load divided by the reported logical CPU count;
it is not CPU utilization. Memory and disk capacities use GiB (2³⁰ bytes). Disk roots retain
their identities even when their capacities match. Disks have no historical series, and the
local in-process Node has no remote round-trip time. Monitoring read errors, no samples, gaps
and stale snapshots are displayed separately.

## Conversation digest base

The Hub's deterministic intake checks stored channel spaces and mail threads once a minute. It does not fetch connector history, start a harness, call a model or send messages. Conversation identifiers are `space:<Steve space ID>` and `mail_thread:<Steve thread ID>`; packet facts use Steve's message, contact and item IDs. Other source types need an explicit reader/access adapter, not a provider-specific ID guessed by a model. The owner's source visibility applies: another member's personal channels and private mail are not intake sources. Digest REST endpoints require the workspace owner; workspace membership alone is insufficient.

### Profiles and independent windows

Every conversation has its own cursor, window sequence, daily spend and settings. Kinds are `project`, `team`, `feed`, `dm`, `mail`, `pr` and `issue`; the currently supported readers are spaces and mail threads. Rules infer DM, mail, bot-heavy feed or team until an explicit classification replaces them. Role reuses the channel's existing duty, rather than introducing a second role field.

All kinds default to **50 messages**, a **30-minute quiet period** and **24-hour maximum wait**. A window is due when any threshold is reached. `window` accepts 5–2000, including 500; each large window is split by estimated tokens, preferring reply/topic boundaries and time gaps, with a small context overlap. Default chunk budget is 6000 estimated tokens including the rendered header/context. One individually oversized message remains whole, so can exceed that estimate; this is not a model tokenizer or a hard billing limit.

`PATCH /api/digest/conversations/{conv}` accepts `kind`, `kind_source` (`you|model`; `rule` is the derived default), `window`, `quiet_ms`, `max_wait_ms`, `depth`, `daily_budget`, `priority` (0–3), `notify` (`now|digest|record`), `notify_source` (`learned|you`; `default` follows the kind) and optional `revision`. Time values are milliseconds. A missing setting is unchanged; explicit JSON `null` removes its override and restores the kind default. A stale supplied revision returns 409. For example, `{"window":500,"depth":"full"}` overrides only that conversation; `{"window":null}` restores 50. Quiet time can be 1 minute–24 hours, maximum wait 10 minutes–7 days, and budget 0–10000000 (0 means unlimited). Classification/notification source labels are stored provenance, not evidence that a model or learning worker has run.

Depth is `full`, `summary`, `alerts`, `record` or `paused`. Full/summary queue packets for a later consumer; alerts folds machine firing/recovery events into a rule summary and queues only unresolved alerts/human discussion; record receipts messages without a packet; paused keeps messages waiting. Muted/blocked sources use record behavior. Default daily budgets are 300000 for team/project, 200000 for DM/mail and 100000 for feed/PR/issue. Budgets are soft: after a window takes spend to the limit, later windows wait for the next Hub-local day. Manual cuts bypass that budget, not source visibility or paused/record depth. Notification choices are persisted only; this base does not deliver digest notifications.

Packets, chunks, per-message receipts and cursor advancement commit atomically. Receipts, not a timestamp watermark alone, prevent duplicates while accepting equal-time, late-arriving and backfilled messages. Default intake includes all stored history. A positive initial lookback is fixed when the conversation first runs, including when paused: it does not slide forward while a backlog waits.

### Queue settings and recovery

The workspace `digest` setting is a JSON object, managed through the existing workspace settings mechanism. Omitted fields use these defaults:

```json
{"enabled":true,"every_min":30,"packets":20,"chunk_tokens":6000,"overlap":2,"context":3,"backlog_days":0,"lease_min":120,"max_attempts":3}
```

`enabled:false` stops scheduled intake. A manual cut is still available. `backlog_days:0` means all stored history; a positive value sets the initial lookback floor for new cursors. It never downloads history. `every_min` and `packets` determine when queued work is due. Once an ordinary Digest Bot schedule is bound (below), a lightweight Hub check can start that schedule for a due batch. Without a configured Bot, packets simply remain queued.

The Rust queue contract exposes due/lease/renew/chunk completion/packet completion. Priority orders eligible packets; a conversation's earliest unfinished packet must finish before its next one can lease. Leases fence stale workers; expiration counts as an attempt. Retries preserve finished chunks, and a packet cannot finish while any chunk is unfinished. Exhaustion leaves `failed`, which intentionally blocks later packets of that conversation. The owner can inspect it and explicitly `POST /api/digest/packets/{id}/retry`; only failed packets reset their attempt budget, with completed chunks and message receipts retained. Restarting the Hub does not reset the queue.

### Owner API and storage contracts

All paths below are relative to `/api`, use the existing authenticated workspace selection, and do not initiate model/network work:

| Method/path | Contract |
| --- | --- |
| `GET/PATCH /digest/conversations/{conv}` | Conversation facts, effective profile/overrides, cursor, latest summary, queue counts; update settings |
| `POST /digest/conversations/{conv}/cut` | Cut currently stored pending messages in this conversation |
| `GET /digest/packets`, `GET /digest/packets/{id}` | Inspect durable packets/facts/candidates and chunk progress; list accepts state/conv/limit |
| `POST /digest/packets/{id}/retry` | Explicit recovery of a failed packet; returns whether it changed |
| `GET/POST /digest/matters`, `GET/PATCH /digest/matters/{id}` | Search/read/create/edit a matter; title, summary and open/done/dropped state |
| `POST /digest/matters/{id}/relate` | `op`: merge/not_same with `other`, split with `segments`/`title`, link/unlink with item/project `object`; explicit `force` can override not_same on merge |
| `GET/POST /digest/todos`, `POST /digest/todos/{id}` | Owner's personal todos, separate from Bot items; create text/matter/due/refs/parties; transition with state/item |
| `GET/POST /digest/memories/{id}/index` | Read/link an existing visible memory's conversation/matter/parties/range; does not create a separate memory system |
| `GET /digest/changes`, `POST /digest/changes/{id}/undo` | Inspect/undo recorded digest changes, refusing incompatible later edits |

Todo `refs` are pairs such as `[[{"type":"space","id":"sp_example"},"cm_example"]]`; a relate `object` is `{"type":"item","id":"ST-12"}`. The memory index POST uses `conv` as a conversation key, optional `matter`, `parties` and the exact `range` object.

Matter retrieval returns a cross-conversation segment timeline, participant counts and item/project links. Split/merge operations retain explicit not_same boundaries, including those involving a merged alias. Segment storage validates **positive packet/chunk positions** and derives message IDs, times, people and chains from persisted facts; context positions are never writeback evidence. Candidate retrieval uses same chain/topic, referenced items, nearby participant overlap and SQLite matter/memory full text (Chinese bigrams included). It supplies candidates, not model decisions. Memory candidates respect owner visibility, expiry and the `never` disclosure level.

Memory links use the existing `mems`/`mem_about` store, plus exact message ranges (`conv`, `first_id`, `last_id`, `first_at`, `last_at`); referenced entities and range endpoints must exist in the workspace. `steve_memory_list` keeps its existing human-readable about field and additionally returns structured `index.about`/`index.ranges`. Matter detail respects the reader's memory visibility, including other members' private memories. Mail-thread references and their displayed subjects use the reader's source visibility even in shared memories or notes; viewer-free search indexing does not copy mail subjects into shared text. Personal todo states are open/done/dropped/handed; handed links an existing workspace item, never automatically creates or starts a Bot. Automated-source deadlines require quoted words actually present in the referenced message. The base includes guarded undo for recorded summary/segment/todo/memory changes; memory-index undo returns 409 without changing data when a later range still relies on its conversation link or a matter merge has retargeted its link. Matter editing has its own event history, not a universal undo for every action.

### Set up the ordinary Digest Bot

The Digest Bot is opt-in, not a separate completion worker or model backend. It uses the same installed harness, ACP session and tool discovery as any other Bot. No compiler, source checkout, private gateway or provider-specific account is part of this contract; the selected harness still needs its own working model access.

1. As the workspace's digest data owner, `GET /api/digest/bot`. It returns `schedule_id` and a `template` containing ordinary Bot rules and schedule fields.
2. Create an ordinary Bot with `POST /api/bots`, using `template.bot` and a configured `harness`. Keep the supplied rules: read source material as data, discover tools, confirm candidate associations, write and inspect their results, and report persisted changes. Save the returned Bot ID.
3. Create an ordinary schedule with `POST /api/schedules`, using `template.schedule`, `bot` set to that ID, and **`requester` set to your authenticated user ID**. Use `item` or `item_auto` and `overlap:"skip"`; the template uses `*/30 * * * *`, missed-run skip, and item_auto. The normal schedule budget and item acceptance settings still apply.
4. `PUT /api/digest/bot` with `{"schedule_id":"<returned schedule ID>"}`. Only the owner can bind a schedule, and binding verifies its requester, workspace, references, item mode and skip-overlap policy. Set `schedule_id:null` to remove the queue-driven binding. Disable the ordinary schedule as well if its cron should stop.

The Hub checks bound queues once a minute and starts due work through the ordinary schedule engine, avoiding a new queue-driven start while that schedule has an active flow run or unfinished item. The schedule's ordinary cron remains a fallback and may create a no-work session; the batch tool reports no due packets and the Bot stops. Binding is not a second harness runner. Disabling the schedule or digest batching stops queue-driven starts; already leased work can still finish or yield.

**Execution identity is explicit, not inferred from member count.** Schedule rows retain their creator, but creation alone does not grant personal execution. Old schedules and new schedules with missing/null `requester` continue to run with workspace service access. A member can authorize only their own ID; only that requester may edit a personal schedule, and execution rechecks membership. The requester is pinned in the schedule graph and propagated to `items.requested_by` and the existing session principal chain; an old graph cannot borrow newly granted personal access. `report` mode does not accept requester because its one-shot report has no personal tool session. Deleting a Bot disables and revokes its personal schedules rather than transferring that authorization to its heir; service schedules keep their existing handoff. The digest intake currently builds owner-facing packets: the tools require that owner's real caller identity and recheck source visibility, including in multi-member workspaces. They never turn a service principal into the owner. All ordinary module tools, including Knowledge and Contacts, use the same authenticated session principal; service callers see workspace-visible data, not an owner's private knowledge or profile. Item opening knowledge uses `items.requested_by` and live membership, never an audit actor or a sole-member guess. A revoked member loses personal tool access even when the session recorded their earlier identity. Contacts does not create a fake person for a service principal.

### Tool-mediated organization and recovery

Tools are discovered with `steve_tools_search` and executed through `steve_tool_call`; they do not enlarge the small core tool list and do not depend on a harness brand.

| Tool | Contract |
| --- | --- |
| `steve_digest_batch` | Lease due packets across conversations; repeating in the same item/session resumes its live batch. Optional `limit` defaults to 5 (1–20). |
| `steve_digest_read` | `packet`, `batch`, optional `chunk` (numbered from 1). Overview returns chunk progress; selecting a chunk supplies its text and relevant deterministic metadata. Saved segment summaries are paged 100 at a time with `segment_after`, `segments_more` and `segment_next`. Previous summary includes its version. |
| `steve_matters_search`, `steve_matter_read` | Search then read existing matters/timelines before assigning them; memory search/read uses existing knowledge and memory tools. |
| `steve_digest_chunk` | Atomically save an entire chunk's `segments`, optional `todos` and `memories`, and acknowledge it. Cover every positive source position, including noise. Context/overlap cannot be written. Segment tags are decision/promise/request/alert/chat/noise/fact. Existing IDs must resolve; a new matter uses `new_matter` title and returns its module-generated ID. Stable chain IDs reuse earlier matter assignments; conflicts require splitting the segment. |
| `steve_digest_finish` | After all chunks finish, supply a complete `summary` and `expected_version` (0 when absent); summary and packet completion commit together. Stale versions require rereading and recombining. Rule-based alert summaries are retained. Optional first-pass `kind` cannot override an existing user/model classification. |
| `steve_digest_control` | `action:renew` extends a live lease; `retry` releases a failed attempt; `yield` releases for another session without consuming a failure attempt. Finished chunks remain saved. |
| `steve_digest_report` | Page this run's persisted changes with `after` (offset), `more` and `next`. Give the owner change IDs and a concise report of associations, todos, candidates, deferred work and failures. Undo uses the owner change API above and refuses incompatible later edits. |

Segments use `first`/`last` positive packet positions. Todos use `text` and `positions`, and only explicit source words justify a `due` timestamp (`due_quote`). Todos are the owner's personal list with `source:"steve"`, not Bot work items. Memories use source `first`/`last`: `id` indexes an existing visible memory; without it `text` and optional `kind` propose a private candidate, and optional `supersedes` references a visible confirmed memory for the existing confirmation flow. Both todos and memories may link an existing `matter`, or use `matter_at` (a position of a segment in the same submission) to link the newly created matter without inventing its ID. Summary-only packets forbid these associations. IDs, participants, message ranges and chain identity are derived or validated by modules, not trusted from model prose.

The Bot reads and judges multiple conversations in one session but saves each chunk separately. For a large window it reads persisted segment pages, combines them with the previous summary and publishes **one complete-window summary**, never one summary per chunk. A later item/session resumes done chunks after yield, failure or expiration. A stale lease or a different run cannot write; repeated done-chunk submissions are no-ops. Failure before a chunk transaction commits leaves neither its writes nor its acknowledgement. The deterministic alert rules still count categories, firing/recovery and handlers; only queued anomalies/human discussion reach the Bot. No automatic history pull, attachment-body download, output-notification pipeline or dedicated messages page is implemented here.

## Outbound checks

Apart from fixed receipts no model wrote ("received, recorded as …"), only two kinds of output go without a model's review. What you and Steve say to each other in your private chat is not reviewed at all. A Bot's reply to an audience made only of workspace members skips the model check when every piece of knowledge it used is public to them; the rule check still runs.

Every other outgoing text, including one sent in your name to a third party (a message from your own account or on your behalf, a mail), passes the rule check (names, amounts, links, file names and item numbers that only restricted knowledge holds) and a second model other than the one that wrote it (`STEVE_MODELS_CHECK`). A flagged span, or a check model that fails or gives no readable answer, holds the text for you. One gap remains: a mail draft written while YOLO is off has the rule check only; its card says so and offers the model check, and YOLO never sends an unchecked draft. Mail sent as you will always run the model check (being implemented, [#247](https://github.com/Infranite/steve/issues/247)). YOLO speaks as you only with the `[AI]` mark, and to an audience with people outside your organisation only when its settings allow it, which they do not by default.

On your own account, trigger words (`/ai`, `/bot`) from anyone but you are screened by a fast model first. A request for your personal details, the deployment or Steve's prompts gets a fixed refusal, and you are told.

## Output intents

The source-reply tool (`steve_reply_source`) now submits to the existing channel outbox through a durable intent ledger. It uses the session's recorded principal, re-resolved against **live workspace membership**. Service work does not borrow the workspace owner's personal access. The same backend is available to authenticated clients:

- `POST /api/deliveries` accepts the intent below and returns `{id,message_id,space_id,connection_id,status,decision_id,attempts,last_error,replied_at,reply_message_id}`. `key` is a stable identity for one semantic send, scoped to the requester and workspace. Reuse it after a timeout or restart; choose another key for a genuinely new message. A reused key with different intent data returns a conflict. Empty content and content exceeding 4,000 characters are rejected, not truncated.
- `GET /api/deliveries/:id` returns the authoritative state from the channel message and retained ledger. Only its original, still-active requester can read it. Repeating `POST` returns this same record and never retries the transport.
- `PATCH /api/people/:party/output-preference` accepts `{"space_id":"<Steve DM ID>"}` or `{"space_id":null}` to set or clear the caller's manual preference for that person. It must be a visible, live, reachable direct conversation containing that person's known account alias. Learning updates a separate value and cannot overwrite a manual setting. An unavailable manual destination is an error, not permission to fall back to another channel.

Example body (identifiers are Steve IDs, not provider IDs):

```json
{
  "key": "item-progress-1",
  "identity": "bot",
  "source_message_id": "cm_source",
  "content": {
    "title": "Progress",
    "markdown": "The requested work is complete.",
    "fields": [["Result", "Ready"]],
    "actions": [["Open result", "https://example.org/result"]],
    "references": [["Original request", "https://example.org/request"]]
  },
  "reason": "item:example",
  "urgency": "normal"
}
```

`identity` is `bot` or `as_you`, and must match the resolved account. An `as_you` account must belong to the requester. Supply `source_message_id`, `space_id`, or `party_id`. Replies default to the source conversation even if a preference exists. An explicit `space_id` switches destinations; when `party_id` is also supplied, that person's stored alias must belong to the destination. A new message to a person considers known active DMs, manual preference first, then learned preference, then the first reachable Steve conversation ID in lexical order. There is no model-based routing, discovery of unknown recipients or automatic creation of DMs. A cross-channel send keeps its source ID in the audit/ledger but never forwards the source platform's reply ID into a different conversation. Include any desired public source link in `content.references`; the Hub does not invent provider links.

An existing source-reply key retains its original identity even if the conversation voice changes before a retry; the original live requester and complete intent fingerprint are still checked. Explicit channel cleanup also covers this ledger: text cleanup removes its content copy while retaining the fingerprint, route and receipts; full cleanup removes deliveries owned by deleted messages/spaces, detaches deleted reply IDs and clears deleted channel/contact preferences in the same transaction. Protected messages retain the existing cleanup exemptions.

The optional `key`, `space_id` and `party_id` arguments are also available on `steve_reply_source`. With no explicit key, identical text from the same session and item uses a deterministic key. Its successful result is the delivery JSON above, not a claim that a queued or drafted message was sent. An item with no chat source still returns the existing no-source answer.

Content is platform-neutral: Markdown plus a title, fields, actions and references. This first path renders a lossless Markdown/plain-text fallback, including labelled action URLs. The existing channel adapter performs platform rendering; the Hub contains no new platform-specific dependency, completion runner or masking. The existing disclosure check and authorization still run. A bot send requested by a live member is authorized as a bot (not through personal approval tiers); personal sends retain the existing approval card, YOLO handling and undo window. The requester, recipient membership, account access and exact stored route are checked again by the queue before sending; a changed conversation voice is not permission to send as another account.

The intent, pinned route and channel message commit in one transaction. The channel outbox remains the **only sender**, with the same message ID as transport dedupe key. Attempts and last errors remain in the ledger after temporary send state is removed. `unknown` means the send may already have happened: repeated submissions never resend it, and the existing queue can repeat only the frozen message within an adapter's declared provider-deduplication window. With no such guarantee, or after its window, the existing uncertain-send decision asks the user to reconcile it; it is not converted to a fresh send. Confirmed failures use the existing retry/discard decisions. Recall uses `POST /api/chan-messages/:message_id/recall`, when the adapter supports it. Real, non-history inbound replies with the outbound provider message ID record a reply receipt; direct-message replies also teach the channel preference, independently of manual overrides.

Current scope is **source replies, explicit intent submissions and opted-in personal work-completion notifications over channel adapters**, not all output workflows. Arbitrary connector-operation writes, email drafts, GitHub writes, legacy generated replies and flow actions retain their existing paths; they have not all acquired this ledger or a single unified send-tier contract. Other notification events, urgency/escalation routing, scheduled digest subscriptions and digest generation remain unimplemented. Feed is a separate reading surface, not a sender or delivery ledger. `urgency` is recorded, not a routing or escalation policy. Provider read receipts, reaction/button receipts and automatic reply-wait completion are not implemented; observed replies and existing adapter recall are connected. Personal Feed read state is separate from provider read receipts. Ordinary database startup applies the migrations; no new service or credentials are needed.

## Feed

**Feed** is the web client's independent reading surface, available in the sidebar and the phone's bottom navigation. It keeps ordinary proactive content separate from chat and from decisions in the Inbox. The first producers are scheduled/automated reports and explicitly opted-in personal work-completion notifications. It does not collect RSS, create new summaries on refresh, expose raw flow steps or import private conversations.

New Report actions publish their already-generated Markdown to a workspace-visible Feed entry instead of adding the full report to the Steve conversation. This preserves the reports' existing shared-workspace audience; each member has separate read state. Existing Report configurations use this same destination without selecting a private recipient. Their `requester` is not repurposed as a subscriber. Historical chat reports stay in chat, with no automatic backfill, deletion or re-send. Report publication and consumption of its selected digest material commit together. A stable run/node source key prevents duplicate entries; recovery of an already-published report returns the original result without generating another report or consuming new digest material. A publication failure does not fall back to chat.

Personal completion entries are visible only to their explicit recipient and reuse the existing notification's body rather than retaining a second copy. Source cleanup or the 30-day notification retention clears the readable content; a tombstone preserves idempotency. Existing personal read state is carried into Feed. Reading never alters notification consent, `now/digest/log`, quiet hours, send state or uncertain-send reconciliation. A shared report being visible does not grant access to a private resource it references; source links retain their own authorization checks.

All Feed routes use the authenticated caller and `x-steve-ws` workspace; there is no arbitrary recipient-user parameter or public producer endpoint:

- `GET /api/me/feed` accepts optional `kind=report|completion`, `source=flow|notification`, `unread=true|false`, `cursor`, and `limit` (default 50, range 1–100). It returns `{entries,next_cursor,snapshot,unread_count}`. The count covers currently visible unread entries in the same kind/source filter, including new arrivals after the page snapshot. Pagination has no 100-entry total cap. Cursors preserve the snapshot and filter scope and expire after 24 hours; invalid or expired tokens or filters return 400 rather than silently restarting pagination. Refresh from the first page to obtain a new snapshot.
- Each entry contains `id`, `seq`, `kind`, `title`, `excerpt`, `body`, `published_at`, `read_at`, `purged`, and `source`. Timestamps are Unix milliseconds. `source` contains `type`, `id` and nullable `run_id`, `node_id`, `event_id`; these are Steve identifiers, not credentials or inferred provider URLs. List entries omit full content (`body:null`), which alone does not mean the entry was purged.
- `GET /api/me/feed/:id` returns a visible entry with its full platform-neutral `Content` (`title`, `markdown`, `fields`, `actions`, `references`), or a cleared-content tombstone. Missing or inaccessible IDs return 404. Fetching a detail does not itself mark it read.
- `POST /api/me/feed/read` takes either `{"ids":["<entry-id>"]}` for a bounded selection or `{"snapshot":"<server-token>","kind":"report","unread":true,"source":"flow"}` for a filter-wide operation. The latter's filters must match those used to obtain the snapshot; omitted filters retain their default meaning. It returns `{"updated":number}`. An inaccessible selected ID rejects and rolls back the whole selection. A snapshot-wide mark affects only matching, currently visible entries at or below that snapshot: concurrent new arrivals remain unread.

Opening the list does not mark everything read. The UI offers unread/source filters, full Markdown, selected and snapshot-wide read operations, and “Load more”. New arrivals prompt a refresh instead of moving the reading position. Failed reads or writes offer retry without claiming success. API requests stay bound to the Hub and workspace in which the page loaded; account/workspace switches discard old resources and cursors. The existing workspace-wide SSE must not broadcast personal entries or personal read state; the reading page refreshes on focus and at bounded intervals while visible. Browsing does not call a model, consume digest inputs, send a message or resolve an Inbox decision.

## Personal attention for desktop clients

`GET /api/me/attention` is an authenticated, caller/workspace-scoped reading projection. It keeps four categories separate: final Steve replies (`replies`, messages), actionable Inbox cards (`pending`, cards), personal Feed content (`feed`, entries), and connected-channel unread (`channels`, conversations). There is no combined message total. It does not create messages, send notifications, approve decisions or mark anything read. Personal projection and reading data are never broadcast to workspace SSE.

The request accepts `kind=replies|pending|feed|channels`, `limit` (default 50, range 1–100), and an opaque `cursor` for continued reading of one category. Omitting `kind` returns each category's first page. The response carries protocol version `v`, an installation `epoch`, `user_id`, `workspace_id`, and `categories`. Each category reports its availability, count and unit, revision, entries, snapshot, and optional continuation cursor. Entry IDs and event IDs are stable source identifiers, not counter changes. Targets name an internal route and optional entity; they are navigation hints, not commands or arbitrary URLs.

Counts cover the category, not just the returned page. Read all pages before treating a category snapshot as a complete baseline. Intermediate pages omit `snapshot`; only the final page issues a completed snapshot capability. A page cursor cannot be used as a completed snapshot or for reply reading. Snapshot cursors are bound to caller, workspace and category, expire after 24 hours, and are not incremental event cursors. If the projected membership changes during pagination, a conflict requires restarting from the category's first page; do not combine pages from different revisions. A category-specific `if_snapshot` request can report an unchanged snapshot without transferring its entries again. An unsupported category is not a known zero.

Final reply tracking starts with the attention migration (`history_policy: since_attention_v1`); old conversation history remains readable and is not retroactively made unread. Only committed final replies addressed to the caller qualify, not streaming drafts, tool output or reasoning. `POST /api/me/attention/read` takes `{"snapshot":"<replies snapshot>","through_id":"<last actually viewed reply ID>"}`. The reply must belong to the caller and the snapshot; its personal reading position advances monotonically through that reply, leaving concurrent later replies unread. A focused window alone is not evidence that the latest reply was visible. Showing, opening or dismissing a desktop notification must not perform a blanket read. `GET /api/me/attention/replies/{id}` resolves a caller-owned committed final reply to its ID and creation time for navigation into conversation history, even after it is read; unknown, foreign and non-final replies return 404.

Pending cards retain the existing authorized workspace decision visibility; they are not a claim that each card is personally assigned to the caller. They use the actionable-card exclusions rather than all open decisions: snoozed and archived items and memory-confirmation cards do not contribute. Reading a reminder does not resolve a card. Feed entries retain the existing `/me/feed/read` state and consent rules. Connected conversations retain their own visibility and reading operations, and their count is conversations, not messages. Channel participation/automation quietness is not a personal mute setting. A channel entry's desktop delivery eligibility is separate from whether its unread indicator is visible; clients must respect explicit local notification preferences and must not announce all group activity by default.

Desktop clients should establish their first baseline silently, retain scoped stable-ID delivery checkpoints, and avoid historical floods on reconnect. This API itself does not add APNs, WebPush or a background service. Older Hubs can lack the endpoint; a client must show that limitation without fabricating unread counts.

## Personal work-completion notifications

The personal notification policy in **Settings → Notifications** consumes the real durable `work.item.done` fact emitted by an item's closing transaction; its entries are read in **Feed**. The notification uses the fact's title, project, cycle and revision, not a later mutable item snapshot. There are no inferred recipients: the signed-in member must explicitly enable their own policy. Workspace owners, item owners, sole members and service callers do not stand in for personal consent. Enabling a policy does not backfill facts older than that consent.

All routes are authenticated and scoped to the caller's current workspace and user; there is no user-id parameter:

- `GET /api/me/notification-policy` returns the complete policy (default: disabled, `log`, UTC, no route or quiet hours).
- `GET /api/me/notification-targets` returns `{"targets":[{"party_id":"…","space_id":"…","party_name":"…","space_name":"…","channel":"slack"}]}` for explicit `now` selection. All five fields are strings; `channel` is the public system identifier (for example `feishu` or `slack`), not a connection credential or account handle. Results are unique and sorted by `party_id`, then `space_id`. The caller must still be a real workspace member. Only their active, visible, user-linked self party and its active personal member accounts qualify: a DM must contain no recorded third person, and its effective sending identity must be an active, visible bot account, not a self account with a bot-mode override. Disabled accounts/connections, departed identities, blocked/non-disclosable conversations and inaccessible routes are excluded using the existing E1 route checks. E1 may retain a route while its connection is paused or needs recovery; listing is not proof that a transport can send immediately. There is no owner/sole-member fallback, implicit self linking, historical-sender/counterpart inference, credential access, history pull, new DM, policy change or send. An unlinked self party returns `{"targets":[]}`. Use both IDs in `PUT /api/me/notification-policy`; normal live validation still runs when saving and sending. Empty person/conversation names receive localized labels in the UI. An empty list makes immediate mode unavailable; a failed lookup offers a retry rather than guessing a route.
- `PUT /api/me/notification-policy` replaces that member's policy. Unknown fields are rejected. Example:

  ```json
  {"enabled":true,"mode":"now","party_id":"<your user-linked Steve party ID>","space_id":null,"timezone":"Asia/Shanghai","quiet_start":1320,"quiet_end":420}
  ```

  `mode` is exactly `now`, `digest` or `log`. `timezone` is an IANA zone. Quiet-hour boundaries are distinct local minutes from midnight, 0–1439; both must be present or both `null`. The interval includes its start and excludes its end; the example means 22:00–07:00 across midnight. Zone rules, including daylight-saving transitions, are used rather than a fixed UTC offset.
- `GET /api/me/notifications` retains the notification-specific newest-100 projection (`id,event_id,mode,body,state,delivery_id,seen_at,created`), with `seen_at` derived from the same personal Feed read state. Use Feed's cursor API for continued reading. Only this member's entries are returned. `waiting` means recoverable now work, `held` means record-only policy, and `submitted` means E1 owns its delivery; it does **not** mean sent. Follow a non-null `delivery_id` through `GET /api/deliveries/:id` for the authoritative C6 status. Cancelled/purged entries do not resend.
- `POST /api/me/notifications/:id/seen` delegates to that caller's Feed read state, idempotently, rather than maintaining a second reading status. Another member's ID returns 404; reading one person's entry never marks anyone else's entry. This does not use workspace-wide notice dismissal or external provider read receipts.

The settings form loads the complete policy and the caller's `/api/me/contact`, never an owner's identity or a hand-entered ID. Its policy, identity, targets and save requests stay bound to the Hub and workspace in which the form loaded, including while a workspace switch is still refreshing. It submits the complete policy only on Save, reports failures without claiming success, and permits disabling even if identity/target discovery is unavailable. For immediate mode, the selected target supplies **both** `party_id` and `space_id`; a saved target that is no longer listed remains visibly unavailable until the member explicitly chooses another. The form never silently picks a replacement or falls back to automatic routing. The existing `/api/spaces` `counterpart` can be inferred from a single member or old message, and its member count may be cached; neither proves exclusive self-and-bot membership or E1 reachability, so these fields are not used as notification targets. The dedicated targets endpoint is required for immediate mode; a failed lookup offers retry and does not guess a route. The legacy workspace `quiet`/`digestAt` settings (`live:false`) are not read or written by this form and do not schedule summaries. No notification-specific conversational configuration tool is claimed.

`now` requires an active party whose `user_id` is exactly the signed-in member, and a live bot DM containing only that person and the sending bot account. `space_id` explicitly selects the DM; if absent, E1's personal manual preference, then learned preference, then deterministic reachable bot DM selection applies. A stale manual preference is an error, not permission to silently switch channels or identity. No new DM is created and no account/owner guessing or identity linking happens automatically. `digest` and `log` retain entries only and require no external route; neither creates a delivery or invokes a sender. They do not generate or schedule digest summaries. Before submission, a manual policy change to digest/log permanently parks waiting now entries rather than letting them burst out on a later change back. An already submitted C6 message waits while digest/log is active; returning to now can resume that same immutable delivery if its consent and route still validate.

The dispatcher commits each event/member entry and its policy outcome with the existing outbox receipt. A bounded recovery worker runs every 10 seconds and submits stable event/member keys to E1. Inbox-to-delivery linkage commits with the E1 intent and C6 message, before external I/O. Replays/restarts therefore find the same receipt; `unknown` is never blindly resubmitted. Quiet hours accumulate now work without invoking E1; already queued C6 messages also wait, then resume under the original key. Existing disclosure checks, approvals where required, ambiguous-outcome reconciliation and channel connection permissions remain unchanged. If a check cannot run, the existing C6 draft/decision path still applies; opt-in is not a bypass. Disabling consent, leaving the workspace, unlinking the self party or losing the route is checked before send and retry. Disabling permanently cancels pending entries; re-enabling does not resurrect them. Changing a submitted route does not silently reroute its immutable delivery.

Notification bodies are retained for 30 days, after which the recovery sweep clears the body and leaves a non-sendable idempotency tombstone. Ordinary channel retention and explicit channel text/full cleanup also clear linked inbox body copies; deleting an E1 delivery detaches its inbox FK without resurrecting the notification. Existing protected-message cleanup exemptions remain in effect. Digest/log entries have no transport FK and follow their own 30-day body retention. Consent references are identifiers, not foreign keys that obstruct channel/contact cleanup; a deleted route must be selected again before further sending.

This is not the whole notification roadmap: other event types, scheduled digest generation/subscriptions, learned notification policy, escalation, upgrades and email delivery are not connected here. Feed provides reading for reports and these existing opted-in completion entries, not new subscriptions or new notification channels. Websocket/SSE notices remain their existing non-durable UI mechanisms; there is no APNs or WebPush sender.
