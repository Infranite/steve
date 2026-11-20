Needs a minor release: the storage-layout conversion is one-way and older releases cannot open the converted database.

Keep the completed pre-upgrade backup. On its first start, the Hub automatically converts databases from v0.5.0 (legacy schema 113 or 114); older databases are refused and must first be opened by v0.5.0. New installations initialize the current layout directly.

Conversion deletes non-owner users' access tokens, registered clients and authentication sessions, together with dependent authentication data. Independent records are retained.

Reinstalling an older release does not undo this conversion. If startup fails, install the release that introduced this layout change, restore the pre-upgrade backup, then start the Hub to convert it again. The database permanently retains its old-reader fence; any temporary compatibility wrapper in the release manifest does not permit downgrades.

Returning to the old storage layout requires restoring a backup taken before conversion. Older programs refuse a converted database without running migrations or writing business data. This refusal does not guarantee that database files remain byte-for-byte unchanged: SQLite may create or touch the `-wal` and `-shm` sidecar files.
