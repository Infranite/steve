# TS Hub → Rust Hub

A Hub release builds the Rust Hub whenever `hub-rs/` is in the tree, and `ops/run.sh` starts it.
The release dir still has the TS Hub, and every earlier release is a TS one, so a rollback is the usual
`current` switch. Rehearsed by `ops/test-release.sh` (fixture) and `ops/test-release.sh --real` (real builds).

What changes in a release:

- `release.sh` runs `cargo build --release --locked -p steve-hub --target-dir ~/.steve/build/cargo-target`.
  The binary lands in `releases/<ver>/bin/steve-hub`, and `release.json` gets `"hub":"rust"` plus `hub_sha256`.
  The release log shows which cargo was used and the full cargo output.
  A cold build takes about 2 min and the target dir is about 1.3 GB. Later releases rebuild only the 13 workspace crates (under 1 min).
- The first switch to Rust backs up the database to `backups/pre-rust-<ver>.sqlite3` (schema 3) after the drain.
  That backup is not rotated. If it fails, there is no switch.
  `releases/history.jsonl` records `hub`, `prev_hub`, `backup`, `schema_before` and `schema_after`.
  The Rust Hub migrates 3 → 11 at boot, or 3 → 12 once rust/base2 is merged. 004–011 only add tables.
  012 rebuilds `spaces`, a Rust-only table from 007, to make its connection FK nullable.
  No table the TS Hub knows is changed.
- `STEVE_HUB_IMPL=ts` in `~/.steve/steve.env` makes `release.sh` skip cargo and `run.sh` start the TS Hub.

## Cut over

Do this when no agent turns are running. A manual run has no drain token, so the restart does not wait.

`<branch>` is what gets released: rust/hub once it has merged ops/rust-cutover (and rust/base2).
It must contain main, because the release fast-forwards main to it.

```sh
cd ~/Agents/steve && git status --short                  # main clean
git merge-base --is-ancestor main <branch> && echo ok     # contains main
git show <branch>:ops/release.sh | grep -c rust_build     # has these ops (non-zero)
grep -n STEVE_HUB_IMPL ~/.steve/steve.env                 # remove a STEVE_HUB_IMPL=ts line, if there is one
git worktree add --detach /tmp/steve-cut <branch>               # run the new release.sh from a checkout of the branch
zsh /tmp/steve-cut/ops/release.sh --hub <branch>
git worktree remove /tmp/steve-cut
```

Run the script from a checkout of the branch, not from `~/.steve/current`. The installed TS release's `release.sh` doesn't know
cargo: it would make a TS release that has the new ops, and only the next release would switch.
Also don't run it from `~/Agents/steve/ops`. The ff-merge rewrites that file while zsh is still reading it.

The release fast-forwards main to the branch, builds, backs up, switches and health-checks.
If `/healthz` doesn't answer with the new version within 60 s, it rolls back to the TS release by itself, and main is reset.
The database stays at schema 11 (12 with base2); the TS Hub opens it with a warning (see below).

To go through Steve instead (with the drain), merge first: put `STEVE_HUB_IMPL=ts` in steve.env, release the
branch normally, and you get a TS release with the new ops. Then remove the line, and the next release switches.

## Check after the cutover

```sh
curl -s http://127.0.0.1:8440/healthz                    # {"ok":true,"version":"<new ver>","web_version":"…"}
tail -3 ~/.steve/releases/history.jsonl                  # "hub":"rust","prev_hub":"ts","backup":"…/pre-rust-<ver>.sqlite3","schema_after":"11"
grep 'run.sh: Rust Hub' ~/.steve/logs/hub.log | tail -1
ps -o command= -p $(pgrep -f steve/current/bin/steve-hub)
sqlite3 ~/.steve/backups/pre-rust-<ver>.sqlite3 'SELECT v FROM schema_version'   # 3
```

In the browser:

- log in on https://steve.example.test and on the `.home` origin (OIDC returns to the origin you came from);
- open an item, send a message, and watch the turn stream (SSE `/api/stream`: the session updates live without a reload);
- check that a decision and a notification arrive;
- make a web-only change and run `release-web.sh main`: no restart, and a reload shows it.

## Roll back to the TS Hub

```sh
zsh ~/.steve/current/ops/release.sh --rollback ts        # newest TS-only release: switch, restart, health check, history line
echo STEVE_HUB_IMPL=ts >> ~/.steve/steve.env             # keep the next releases on TS while main has hub-rs/
```

Without the script:

```sh
ls ~/.steve/releases                                     # the TS one has no bin/; its version is "prev" in the cutover's history line
ln -sfn ~/.steve/releases/<ts-ver> ~/.steve/current && launchctl kickstart -k gui/$(id -u)/io.github.infranite.steve
```

There is also an in-place fallback in the same release: add `STEVE_HUB_IMPL=ts` to steve.env and run `launchctl kickstart`.
It runs the TS Hub built from the same commit.

The TS Hub opens the newer database unchanged and logs `database schema v11 is newer than this Hub (v3); opening it unchanged`
(or `v12`). TS reads and writes everything it knows, because no TS table is changed.
Schema 12 was checked on 2026-11-01: a DB migrated by the Rust Hub, then base2's 012.sql (`foreign_key_check` clean).
The TS Hub opened it with the warning; `/healthz`, dev login, `/api/state`, `POST /api/items` and SSE worked, `schema_version` stayed 12,
and the Rust Hub reopened it afterwards.
`test-release.sh --real` checks this on the real migrated database: login, `/api/state`, SSE, an item created on TS,
and items going both ways across Rust → TS → Rust.
Data in the Rust-only tables (contacts, vault, automation, channels, knowledge, mail, GitHub) is invisible to TS.
It stays, and comes back with the next Rust release.

Restore `pre-rust-<ver>.sqlite3` only if TS misbehaves on the migrated database.
This loses everything written since the cutover:

```sh
launchctl bootout gui/$(id -u)/io.github.infranite.steve
cp ~/.steve/data/steve.sqlite3 ~/.steve/backups/before-restore-$(date +%Y%m%d-%H%M%S).sqlite3
rm -f ~/.steve/data/steve.sqlite3-wal ~/.steve/data/steve.sqlite3-shm
cp ~/.steve/backups/pre-rust-<ver>.sqlite3 ~/.steve/data/steve.sqlite3
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.infranite.steve.plist
```

Release pruning keeps the newest TS-only release, so `--rollback ts` keeps working after several Rust releases.
