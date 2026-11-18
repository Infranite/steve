The 30-second stall watch now also sweeps every open item (not started, working and closing; archived items are skipped) and works out the one thing it would do to move each item on: wake the agent, raise a card for you, or remind you. This is observe-only. Every action has an execution switch, and all of them are off, so nothing new is sent, raised or changed. Today's stall card and Steve's follow-ups still do everything they did before.

- The decision is recorded once per item, cycle, session, turn, goal version, action, reason and the one thing it is about (an operation, a card or a wait), so a restart neither loses nor repeats it, and the same reason about another build or wait is a decision of its own. A decision that stops holding is marked cleared. If it comes back, it counts as a new episode of the same record.
- Every clock comes from the named work policy. With an open card, the sweep adds nothing.
- It names the stuck states nothing moves on today, each as a card it would raise:
  - a pull request problem the agent was told about and left on the same commit;
  - a merge you declined;
  - a closing waiting for this Hub to run the result;
  - a failed required build;
  - a wrap-up module that went silent;
  - the session's Node gone while closing;
  - an item paused when you dismissed a card, with nothing since;
  - a card you answered on a paused item.
- The item's Debug view shows, under "Derived state (shadow)", what the sweep would do, when it comes due, and its recent decisions. `GET /api/items/{id}` carries the same under `shadow.would_do`.
- A migration adds the empty table for this log. It runs by itself and is fast on a large database.
