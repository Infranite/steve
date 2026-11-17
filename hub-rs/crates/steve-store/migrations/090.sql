-- 90: the design gate is a switch on the item (Q1), no longer guessed from words of its goal. On, a
-- delivery is a design for the owner to confirm (implement, keep the design only, or revise) before
-- any implementation; YOLO never answers that card. Off (every item, existing ones included) by
-- default; only the instance's owner switches it, never a Bot, an MCP tool or YOLO.
ALTER TABLE items ADD COLUMN design_gate INTEGER NOT NULL DEFAULT 0;
