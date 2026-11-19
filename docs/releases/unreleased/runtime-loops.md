Scheduled YOLO actions now follow their saved deadlines, including when storage reads are delayed, and wake their runner after schedule changes commit. A failed action keeps its own 15-second retry, so other actions can run on time; repeated executor errors still trip the existing circuit breaker.

Connection token refresh, health checks and directory sync now follow their next deadlines, including existing health backoff, daily workspace time zones, catch-up and event debounce. Periodic reconciliation no longer replays missed scans in a burst. Folder scan requests leave the full-scan deadline unchanged, and judging still re-reads its configured delay after each pass finishes.

Connection health, vault access, merge, judge and YOLO session-error retries use the shared backoff policy with the same waiting sequences and caps. Profile lookups retain their existing 1/6/24/72-hour schedule.

Token refresh now runs at the saved expiry minus 10 minutes instead of waiting for the next 5-minute polling tick. New connections are discovered by the deadline loops within their 60-second fallback; connection writes do not currently signal these loops. If an individual directory connection lookup fails, its pending request is retained for a retry 15 minutes later while other requests continue; previously a failed tick could discard all pending requests in that batch.

Scheduled YOLO changes wake their runner only after the outermost transaction commits, including writes outside the ledger helpers. Retrying an action retains its original business reason. During maintenance the runner rechecks within the existing 15-second cadence.
