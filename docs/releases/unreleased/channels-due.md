Queued channel sends and personal notifications wake after their database changes
commit, with a shared recovery fallback. Unknown notification topics and deferred
sends no longer keep the queue busy or hold back other due work. Provider retry
limits, retry hints, authorization checks and rate limits retain their behavior.
