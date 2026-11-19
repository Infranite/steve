-- The merge lane and the merge queue are gone: a git project's change lands through its own pull
-- request. Their tables go, and cards of theirs still open are expired (answering them did nothing).
DROP TABLE IF EXISTS mq_steps;
DROP TABLE IF EXISTS mq_stats;
DROP TABLE IF EXISTS mq_entries;
DROP TABLE IF EXISTS mq_batches;
DROP TABLE IF EXISTS mq_trains;
DROP TABLE IF EXISTS mq_targets;
DROP TABLE IF EXISTS lane_requests;
DROP TABLE IF EXISTS lane_units;
DROP TABLE IF EXISTS lanes;
UPDATE decisions SET status = 'expired', resolved = CAST(strftime('%s', 'now') AS INTEGER) * 1000
 WHERE status = 'open' AND subject_type IN ('delivery_batch', 'mq_target', 'mq_batch', 'mq_train');
