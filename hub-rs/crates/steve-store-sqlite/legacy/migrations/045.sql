-- The outbox gives up on a delivery that keeps failing by time, not by count: when its current run of
-- failures began (NULL: not failing, or failing since before this column).
ALTER TABLE outbox_leases ADD COLUMN failing_since INTEGER;
-- Sessions of archived items whose event log was compacted under the workspace's keepSess (images to
-- placeholders, big tool outputs to their head and tail): up to which seq, so a later pass (after the
-- item was taken back up and archived again) compacts only what came after.
CREATE TABLE session_compactions (
  session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
  upto INTEGER NOT NULL,
  events INTEGER NOT NULL,
  at INTEGER NOT NULL);
-- keepSess 90 天 / 1 年 used to do nothing (the old settings page said so); from this release they compact.
-- A value chosen under the old meaning is not a consent to lossy compaction: every workspace starts at
-- 永久 and only a choice made from now on (a confirmed config draft) takes effect.
UPDATE settings SET value = '"永久"', updated = CAST(strftime('%s', 'now') AS INTEGER) * 1000
  WHERE key = 'keepSess' AND value <> '"永久"';
