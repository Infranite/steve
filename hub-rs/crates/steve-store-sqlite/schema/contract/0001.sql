-- Shared physical storage contracts; domain qualifies keys in the single-file layout.
CREATE TABLE meta (
    domain TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
    PRIMARY KEY (domain, key)
);
CREATE TABLE uow_applied (
    domain TEXT NOT NULL, key TEXT NOT NULL, issued_at INTEGER NOT NULL,
    digest BLOB NOT NULL, rev INTEGER NOT NULL, result BLOB NOT NULL,
    PRIMARY KEY (domain, key)
);
CREATE INDEX uow_applied_issued_at ON uow_applied(issued_at);
CREATE TABLE cursor (
    domain TEXT NOT NULL, name TEXT NOT NULL, key TEXT NOT NULL, updated INTEGER NOT NULL,
    PRIMARY KEY (domain, name)
);
CREATE TABLE domain_schema (
    domain TEXT PRIMARY KEY NOT NULL, version INTEGER NOT NULL
);
