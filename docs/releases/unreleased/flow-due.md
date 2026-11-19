Needs a minor release: invalid stored flows may now be disabled or failed instead of silently retried; repair and re-enable affected schedules or retry failed runs.

Flow waits and failed-run retries wake at their own deadlines instead of waiting up to 15 seconds for a trigger poll. Committed todo changes immediately replan reminders, and deferred alarms no longer crowd due reminders out of a batch.

A local claim or planning error now ends only the affected record: its trigger is disabled, its run fails, or its reminder/request is cancelled. The failure is recorded in the audit log. Other due work continues on time; a failed reminder cannot undo other reminders in its batch. Repair the subject, then resave/re-enable its schedule, explicitly retry the failed run, or re-arm a cancelled reminder. Action-execution retries retain their existing policy.

Stored graphs that cannot be parsed permanently disable their triggers. Invalid persisted wait or retry state fails its run instead of being skipped. Planning errors with a 4xx status cancel that subject's planned alarms. Where a stored time zone needs a fallback, the operator's system zone is used rather than Taipei.

Cron evaluation uses the shared runtime implementation and RFC 5545 time-zone rules: a time in a spring-forward gap is shifted using the pre-transition offset, and a repeated fall-back time runs on its first occurrence only.
