-- Add explicit pin flag on memories so prompt assembly prioritizes pinned memories
-- ahead of kind priority and recency within the named character/token budgets.
ALTER TABLE mems ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
