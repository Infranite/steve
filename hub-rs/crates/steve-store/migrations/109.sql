-- Replies reuse immutable delivery objects, but must not become independently shareable.
ALTER TABLE deliveries ADD COLUMN reply INTEGER NOT NULL DEFAULT 0 CHECK(reply IN (0, 1));
