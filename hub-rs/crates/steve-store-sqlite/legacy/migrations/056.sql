-- A pairing code made by the Hub for its own remote install (A6b): the owner authorised the install,
-- so the Node it pairs is active at once instead of waiting for the fingerprint to be confirmed.
ALTER TABLE node_pairings ADD COLUMN confirmed INTEGER NOT NULL DEFAULT 0;
