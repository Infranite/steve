-- How each Node's machine was doing (B3), sampled about once a minute by the Hub and kept for a day:
-- the devices page shows it, and choosing where work goes can read it.
CREATE TABLE node_vitals (
 node_id TEXT NOT NULL,
 at INTEGER NOT NULL,
 rtt_ms INTEGER,
 data TEXT NOT NULL);
CREATE INDEX node_vitals_node_at ON node_vitals(node_id, at);
