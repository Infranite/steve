Digest queue-triggered schedules stop retrying after three startup failures in the same configuration. Any save of the bound schedule or Bot opens a new retry budget, including a name-only edit or disabling and re-enabling it. A bad schedule is isolated from other workspaces, and ordinary scheduled or manual runs stay enabled.

Recognizable digest startup-error markers created by unpublished development builds are removed on startup, along with their steps, waits and run links. Independent items and child runs are retained. Current startup failure history and retry budgets are kept.

Startup rejections show a stable explanation rather than database decoder details; the original error is logged. If startup tracking metadata is damaged, the digest owner can read its code and flow/run location in the diagnostics field of GET /api/digest/bot.

Saving the schedule or Bot also recovers when an older startup budget is damaged. The old record stays available in owner diagnostics, even after a new run starts. Repeated wakeups log a damaged run once per Bot worker lifetime.
