-- N1-6: a Node event that conflicts with what the Hub holds is set aside per stream, not refused
-- with its whole batch. The stream is marked faulted (its apply stops until reconciliation) and the
-- event is kept here durably: the Hub acknowledges it, so this row is the only copy left. Never pruned.
-- An older build ignores the new column and table.
ALTER TABLE node_streams ADD COLUMN fault TEXT CHECK(fault IS NULL OR fault IN ('conflict','gap','ahead'));
CREATE INDEX node_streams_faulted ON node_streams(node_id) WHERE fault IS NOT NULL;
CREATE TABLE node_event_quarantine (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq>0),
 event_id TEXT NOT NULL, payload TEXT NOT NULL, reason TEXT NOT NULL, received INTEGER NOT NULL,
 PRIMARY KEY(node_id,stream,epoch,seq,event_id),
 FOREIGN KEY(node_id,stream,epoch) REFERENCES node_streams(node_id,stream,epoch));
