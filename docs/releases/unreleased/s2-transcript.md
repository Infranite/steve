Needs a minor release: upgrade clients that read session events to handle cursor-paged responses.

Session recordings open at the latest turns. Scroll up to load earlier turns without losing your place. New activity appears live while you are at the bottom. Item sessions use the same on-demand history, and Markdown export includes the full recording.
`GET /api/sessions/{id}/events` returns `{events, next}`. Use `before` to read earlier turns and `after` to resume from the last event received.
