-- Removing a Node from its machine (#325): every removal the owner asked of a Node, with where it
-- is. The Node itself stops taking work, uninstalls its service and deletes its home once the Hub
-- confirms; the Hub then unpairs it. `removed` is the only end that unpaired it.
CREATE TABLE node_removals (
 operation_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 requested_by TEXT NOT NULL,
 grace_secs INTEGER NOT NULL,
 phase TEXT NOT NULL CHECK(phase IN ('requested','draining','cancelling','ready','confirming','removed','unconfirmed','failed','manual')),
 error TEXT,
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL);
CREATE INDEX node_removals_node ON node_removals(node_id, created);
