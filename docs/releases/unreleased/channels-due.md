Queued channel sends and personal notifications wake after their database changes
commit, with a shared recovery fallback. Unknown notification topics and deferred
sends no longer keep the queue busy or hold back other due work. Provider retry
limits, retry hints, authorization checks and rate limits retain their behavior.

Approved sends now wait for the latest linked personal-notification attempt deadline
before becoming eligible, rather than using that deadline only to order the queue.
This usually adds about ten seconds when a notification attempt is pending.
