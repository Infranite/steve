-- 99: 我的待办 become goals and dated todos, for every member. A goal (`kind = 'goal'`) has a target
-- date and child todos one level down (`parent_id`); its progress is computed, never stored. A
-- deadline is a moment (`due_kind = 'time'`, `due`) or a local date (`due_kind = 'date'`,
-- `due_date`, `due` empty) so that a date-only deadline does not move a day between time zones.
-- `remind_at` and `snooze_until` are moments; `block_event` is the calendar time block a todo was
-- scheduled into; `pri` 0 (none) to 3, `star` 0/1; `recur` a small RRULE subset, the next todo made
-- when one is completed. Existing deadlines are moments.
ALTER TABLE todos ADD COLUMN kind TEXT NOT NULL DEFAULT 'todo';
ALTER TABLE todos ADD COLUMN parent_id TEXT REFERENCES todos(id) ON DELETE SET NULL;
ALTER TABLE todos ADD COLUMN target_date TEXT;
ALTER TABLE todos ADD COLUMN due_kind TEXT;
ALTER TABLE todos ADD COLUMN due_date TEXT;
ALTER TABLE todos ADD COLUMN remind_at INTEGER;
ALTER TABLE todos ADD COLUMN snooze_until INTEGER;
ALTER TABLE todos ADD COLUMN block_event TEXT;
ALTER TABLE todos ADD COLUMN pri INTEGER NOT NULL DEFAULT 0;
ALTER TABLE todos ADD COLUMN star INTEGER NOT NULL DEFAULT 0;
ALTER TABLE todos ADD COLUMN recur TEXT;
UPDATE todos SET due_kind = 'time' WHERE due IS NOT NULL;
CREATE INDEX IF NOT EXISTS todos_parent ON todos(parent_id) WHERE parent_id IS NOT NULL;
