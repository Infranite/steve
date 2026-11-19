Needs a minor release: the default time zone now follows the system, and daylight-saving gaps use RFC 5545 semantics.

Schedules, todo reminders and calendar imports use the same DST rule: a missing wall time keeps the offset before the jump, and a repeated time uses its first occurrence. For example, New York's 2026-03-08 02:15 runs at 03:15, rather than 03:00.

The workspace time zone defaults to “跟随系统”; existing explicit choices remain available. New or replanned morning briefs read “每天汇报” (`08:30`, `09:00`, or `不发`) and still run only on weekdays. Changing “每天汇报” immediately reschedules existing morning briefs at the new time.

Calendar imports resolve unknown TZIDs using `X-WR-TIMEZONE`, then the connection's time zone, then the system zone, with a diagnostic instead of silently treating the wall time as UTC.
