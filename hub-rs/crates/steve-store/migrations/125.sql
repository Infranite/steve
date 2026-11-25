-- 125: durable daily aggregates for low-cardinality telemetry counters (starting with web.nav.open).
CREATE TABLE telemetry_daily (
  workspace_id TEXT NOT NULL,
  day TEXT NOT NULL,
  metric TEXT NOT NULL,
  label TEXT NOT NULL,
  value BIGINT NOT NULL DEFAULT 0,
  PRIMARY KEY (workspace_id, day, metric, label)
);
CREATE INDEX telemetry_daily_metric_day ON telemetry_daily(workspace_id, metric, day);
