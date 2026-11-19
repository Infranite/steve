Flow waits and failed-run retries now wake at their own deadlines instead of waiting up to 15 seconds for a trigger poll. Committed todo changes immediately replan reminders, and deferred alarms no longer crowd due reminders out of a batch.

Cron evaluation uses the shared runtime implementation and RFC 5545 time-zone rules: a time in a spring-forward gap is shifted using the pre-transition offset, and a repeated fall-back time runs on its first occurrence only.
