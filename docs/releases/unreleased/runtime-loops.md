Scheduled YOLO actions now use their saved deadlines and wake their runner after schedule changes are committed. Existing terminal outcomes and the 15-second error retry cadence are preserved. Connection health, vault access, profile lookup, merge, judge and YOLO session-error retries now use the shared runtime backoff policy with the same waiting sequences and caps.

Deadline loops now calculate their remaining wait after the storage query finishes, so query latency does not delay due work. Continuing sources can verify row-local retry without inventing terminal outcomes.
