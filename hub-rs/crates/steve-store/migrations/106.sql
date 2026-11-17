-- The completion judge, in shadow (#335). Additive: a new, empty table and nullable columns on
-- migration 104's (still empty) judgements. Nothing acts on either: the judge stores its verdict and
-- what it would trigger, and never closes, reviews, wakes or merges.

-- One judge candidate per (item, cycle, current session, logical turn, scope revision): registered
-- when a turn ends, or by the sweep for a terminal turn that has none. `settled` is the gate's last
-- reading (`evaluable` | `busy` | `unknown`) and `settled_reason` why. `claim` is the token of the one
-- pass that judges it now (one advancer at a time); `input_seq` and `fingerprint` are the inputs it
-- judged, compared again before its verdict is written.
CREATE TABLE item_judge_candidates (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  scope_revision INTEGER NOT NULL,
  terminal_attempt TEXT,
  registered_by TEXT NOT NULL CHECK(registered_by IN ('turn_end','sweep')),
  state TEXT NOT NULL DEFAULT 'pending'
    CHECK(state IN ('pending','judging','judged','superseded','failed')),
  settled TEXT CHECK(settled IS NULL OR settled IN ('evaluable','busy','unknown')),
  settled_reason TEXT,
  claim TEXT,
  claimed_at INTEGER,
  input_seq INTEGER,
  fingerprint TEXT,
  failures INTEGER NOT NULL DEFAULT 0 CHECK(failures>=0),
  last_error TEXT,
  next_check INTEGER NOT NULL,
  judgement_id TEXT,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE(item_id,cycle,session_id,turn_id,scope_revision)
);
CREATE INDEX item_judge_candidates_due ON item_judge_candidates(next_check)
  WHERE state IN ('pending','judging');

-- What a judgement rests on: its candidate, who decided (`facts`: verified facts alone; `rule`: a
-- deterministic rule of the turn; `model`: the judge model), what it would trigger, whether the model
-- that answered was another than the worker's (`yes` | `no` | `unknown`), and the inputs' fingerprint.
ALTER TABLE item_judgements ADD COLUMN candidate_id TEXT;
ALTER TABLE item_judgements ADD COLUMN decided_by TEXT;
ALTER TABLE item_judgements ADD COLUMN would TEXT;
ALTER TABLE item_judgements ADD COLUMN independent TEXT;
ALTER TABLE item_judgements ADD COLUMN fingerprint TEXT;
