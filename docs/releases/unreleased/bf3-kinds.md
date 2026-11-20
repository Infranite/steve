Needs a minor release: category initialization now refuses legacy tags whose reserved category name is used in another tag category.

Before upgrading, rename any non-category tag named bug, feat, doc, refactor, ops or chore. Initialization reports a conflict instead of changing that tag's category and silently adding another category to its items. A refused category initialization leaves the existing tags and items unchanged. If this conflict is in the default workspace, the Hub fails to start; in any affected workspace, task creation, task tag changes and creation of any tag are refused until the conflict is resolved. The error identifies the tag ID, name and current category and asks you to rename it and retry, or rename it offline and restart if the Hub cannot start.

Workflow child creation validates inherited tags through the same single-category check as ordinary task creation. An invalid parent category set must be corrected before dispatch; refusal does not create a child or consume its item number. No historical category backfill is performed.

While the Hub is running, rename the conflicting tag through the existing tag editor or API and retry; no restart is needed. If the Hub cannot start, recover offline:

1. Stop the Hub and its service manager with `steve hub stop`, using this installation's existing `--home` and `--label`. Keep all writers stopped during the repair.
2. Open the configured home's `data/steve.sqlite3` with `sqlite3`. Use `.backup '/path/to/new-offline-backup.sqlite3'` to create a backup at a new destination before changing anything.
3. Use the tag ID, name and current category from the error. Pick a new name that is unique within its workspace and is not bug, feat, doc, refactor, ops or chore (for example, legacy-chore). Run the following interactively, replacing the placeholders with those values; escape a single quote inside a value by doubling it:

   ```sql
   BEGIN IMMEDIATE;
   SELECT id, workspace_id, name, cat FROM tags WHERE id = '<reported-id>';
   UPDATE tags SET name = '<new-name>'
   WHERE id = '<reported-id>' AND name = '<reported-name>' AND cat = '<reported-category>';
   SELECT changes();
   ```

4. Commit with `COMMIT;` only after checking that the selected row is the intended tag and `changes()` is 1; otherwise use `ROLLBACK;`. Quit SQLite and restart the Hub. If another conflict is reported, repeat for that tag. This renames the existing tag without changing its ID, category or item associations; it does not reclassify historical items.
