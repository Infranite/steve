# TS Hub → Rust Hub (done) and going back

The cutover to the Rust Hub is done (2026-11-01). Every release builds and runs the Rust Hub. The TS Hub is not built,
`STEVE_HUB_IMPL=ts` is gone, and `release.sh --rollback ts` is no longer a target.

There is no way back to the TS Hub. It never checked which schema it was opening: it took a newer database "unchanged, with a
warning" and kept writing to it. After many Rust-only migrations, nothing shows that this is safe. A TS-only release now says
nothing about its schema, so the gate below refuses it.

## Which release may run on the database

A database records the newest schema any Hub that opened it knew (`schema_compat.min_reader`, written at every open and only
ever raised), and only a Hub that knows at least that schema may open it. Every migration counts: no older Hub may use a
database a newer one migrated. A database from before 0.2.14 has no record, so it needs a Hub that knows its own schema.

- The Hub (0.2.14 on) checks this when it opens the database, before it writes anything. It refuses to start otherwise
  (`数据库（schema vN）需要至少认识 schema vM 的版本才能打开…` in the log). Older Hubs don't check.
- So every script that switches `current` asks `ops/schema_gate.py check <db> <release dir>` first. A release's schema is
  `schema.target` in its release.json (written from the binary when the release is built). A release that doesn't say is
  refused:
  - `release.sh --rollback [<version>]` refuses before it drains anything. The YOLO undo uses it, and the Hub applies the
    same rule (`Core::rollback_refusal`) before it offers or starts one, so a refused undo is never shown. The result goes to
    `releases/rollback.json`, not last.json (a Hub from before 0.2.14 would read that as a release); the Hub notes it on
    the item and marks the undo done only when the rollback went through;
  - `release.sh` after a failed health check switches back only if the previous release can still open the database. If the
    new Hub migrated it past that, `current` stays on the failed release (exit 3; `degraded` in last.json and history; the
    Hub reports it as such, and YOLO leaves it to you), and the error names the pre-release backup;
  - `ops/local-release.py deploy`, the way this machine is upgraded, refuses a release that can't open the database. This
    makes it the only way to an earlier release as well;
  - the managed supervisor (`--supervise`) checks the candidate's manifest against the migrated schema, and restores its
    backup if the candidate fails, before anything external has happened.
- `migrations/LOCK` holds the sha256 of every migration file. A released migration never changes (`released_migrations_are_frozen`).

## Going back after a migration

Don't point `current` at a release from before 0.2.14 by hand: those Hubs don't check, and would open whatever they find.

When an older release can't open the database, going back means restoring the backup taken before the release. That loses
everything written since, including external effects that are not undone. So no script does it on its own. Do it by hand,
and only when the newer release can't run at all:

```sh
launchctl bootout gui/$(id -u)/<label>
# keep what is there now, WAL included (.backup reads through it; a plain cp of the main file would not)
sqlite3 ~/.steve/data/steve.sqlite3 ".backup '$HOME/.steve/backups/before-restore-$(date +%Y%m%d-%H%M%S).sqlite3'"
rm -f ~/.steve/data/steve.sqlite3 ~/.steve/data/steve.sqlite3-wal ~/.steve/data/steve.sqlite3-shm
cp <the pre-release backup named in the failure> ~/.steve/data/steve.sqlite3
ln -sfn ~/.steve/releases/<the release that backup came from> ~/.steve/current
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
```

Pre-release backups: `backups/pre-<release>*.sqlite3` (release.sh, local-release.py) and `updates/` (supervisor).
