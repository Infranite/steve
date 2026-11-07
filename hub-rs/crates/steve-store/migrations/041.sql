-- 41: an extension adapter's own poll cursors: an opaque text per connection and container
-- (`chan_cursors.kind = 'ext'`), next to the built-in self poller's create_time cursors.
-- An older build ignores the new column.
ALTER TABLE chan_cursors ADD COLUMN token TEXT;
