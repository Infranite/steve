CREATE TABLE backup_policy (
    singleton BIGINT PRIMARY KEY CHECK (singleton = 1),
    enabled BIGINT NOT NULL CHECK (enabled IN (0, 1)),
    keep_days BIGINT NOT NULL CHECK (keep_days BETWEEN 1 AND 365)
);
