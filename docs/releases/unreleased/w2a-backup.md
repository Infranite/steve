Needs a minor release: Hub backup names and layout have changed; update tools that consume the old flat backup filenames.

Hub daily, upgrade, repair, cleanup and manual backups now use a single verified SQLite snapshot implementation. Each backup lives in a `<reason>-<UTC time>-<id>/` directory with `manifest.json`, the database and optional `files.tar`. The manifest records checksums, the snapshot's schema and whether the best-effort files archive is complete. The Hub no longer creates flat `daily-*.sqlite3` files.

Owners can request a diagnostic backup with `POST /api/system/backup` or `steve-hub --admin backup`; the CLI uses the private management socket, or takes the installation lock when the Hub is stopped. `--reason pre-upgrade` selects upgrade retention; `--reason pre-destructive` records a retained backup before a manual destructive operation. Status includes counts and bytes by purpose. Daily copies are kept for 14 days and upgrade copies for eight backups; diagnostic and pre-destructive copies are retained.

To restore manually, stop the Hub, remove the old database's `-wal` and `-shm` sidecars, and copy the database named in the chosen manifest to `data/steve.sqlite3`; provide any required external vault keys separately.

Installers, source deployments, development snapshots and delivery-repair scripts now request backups through `steve-hub --admin backup`. Deployment receipts name the manifest directory, and failed backup commands stop deployment. Legacy flat backups are left untouched. Source installations use the Hub's daily scheduler; the separate backup launchd script and plist have been removed. Unload an existing backup job and remove its plist as described in the deployment guide.

The shared files archive includes `deliveries` as well as attachments, workspace outputs, signing identity, secrets and `steve.env`; its completeness is recorded in the manifest. Delivery-repair scripts use the retained `pre-destructive` purpose.
