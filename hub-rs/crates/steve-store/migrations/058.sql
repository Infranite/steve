-- Node tags (B7): labels the owner gives each Node, the Hub's own (`local`) included, and the tag an
-- item asks of the Node its sessions run on, so work can be sent where it fits.
CREATE TABLE node_tags (
 node_id TEXT NOT NULL,
 tag TEXT NOT NULL,
 PRIMARY KEY (node_id, tag));
CREATE TABLE item_node_tags (
 item_id TEXT PRIMARY KEY,
 tag TEXT NOT NULL);
