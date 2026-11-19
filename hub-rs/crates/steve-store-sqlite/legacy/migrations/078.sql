-- Durable report identity and meter heads for live attempt accounting. The projection stays in
-- usage_records/stat_daily, so session, item and workspace reads share one source of truth.
CREATE TABLE usage_reports (
  turn_id TEXT NOT NULL REFERENCES agent_turns(id) ON DELETE CASCADE,
  report_id TEXT NOT NULL,
  payload TEXT NOT NULL,
  PRIMARY KEY(turn_id, report_id)
);
CREATE TABLE usage_meters (
  turn_id TEXT NOT NULL REFERENCES agent_turns(id) ON DELETE CASCADE,
  meter_epoch TEXT NOT NULL, model TEXT NOT NULL, scope TEXT NOT NULL,
  payload TEXT NOT NULL,
  PRIMARY KEY(turn_id, meter_epoch, model, scope)
);
ALTER TABLE usage_records ADD COLUMN live_turn_id TEXT REFERENCES agent_turns(id) ON DELETE CASCADE;
CREATE INDEX usage_live_turn ON usage_records(live_turn_id) WHERE live_turn_id IS NOT NULL;
