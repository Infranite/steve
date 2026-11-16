-- Result recovery has its own FIFO: report inbox fairness cannot rotate dirty caller outcomes.
-- AUTOINCREMENT prevents a new arrival or a retried head from reusing a completed tail's ticket.
CREATE TABLE gateway_result_ready (
 ticket INTEGER PRIMARY KEY AUTOINCREMENT,
 call_id TEXT NOT NULL UNIQUE REFERENCES gateway_calls(call_id) ON DELETE CASCADE);
INSERT INTO gateway_result_ready(call_id)
 SELECT call_id FROM gateway_calls WHERE projection_dirty=1 ORDER BY rowid;
CREATE TRIGGER gateway_result_insert AFTER INSERT ON gateway_calls
 WHEN NEW.projection_dirty=1 BEGIN
 INSERT OR IGNORE INTO gateway_result_ready(call_id) VALUES(NEW.call_id);
END;
CREATE TRIGGER gateway_result_dirty AFTER UPDATE OF projection_dirty ON gateway_calls
 WHEN NEW.projection_dirty=1 BEGIN
 INSERT OR IGNORE INTO gateway_result_ready(call_id) VALUES(NEW.call_id);
END;
CREATE TRIGGER gateway_result_clean AFTER UPDATE OF projection_dirty ON gateway_calls
 WHEN NEW.projection_dirty=0 BEGIN
 DELETE FROM gateway_result_ready WHERE call_id=NEW.call_id;
END;
