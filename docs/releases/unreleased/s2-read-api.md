Needs a minor release: the Hub and Web now use API version 2. Reload open Web tabs after upgrading. Apple clients that require API version 1 will report the Hub as incompatible.

- The Web loads workspace configuration and operational status separately from item, session, decision and conversation snapshots. Changes to those views arrive as entity patches.
- Integrations that read the former aggregate endpoint can use scoped snapshots and views for records, the shell endpoint for workspace configuration, or the summary endpoint for health and activity counts.
- Runtime discovery and refresh are available at `/api/runtimes`. A session list uses the session snapshot and the ended-session view page.
