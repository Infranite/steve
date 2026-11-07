-- A prerequisite wait belongs to an item cycle, not to the workspace preference.
CREATE TABLE item_dependency_waits (
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 cycle INTEGER NOT NULL,
 session_id TEXT NOT NULL REFERENCES sessions(id),
 resume TEXT NOT NULL CHECK(resume IN ('automatic','notify','workspace')),
 reason TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('waiting','started','notified')),
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL,
 PRIMARY KEY(item_id,cycle)
);

-- Completion through any writer must respect real prerequisite completion.
CREATE TRIGGER item_dependencies_completion_gate
BEFORE UPDATE OF phase,outcome,status,cycle ON items
WHEN NEW.phase='closed' AND NEW.outcome='completed'
 AND (EXISTS (
  SELECT 1 FROM item_deps d LEFT JOIN items p ON p.id=d.dep_id
  WHERE d.item_id=NEW.id AND
   (p.id IS NULL OR p.status<>'done' OR p.phase<>'closed' OR p.outcome IS NOT 'completed')
 ) OR EXISTS (
  SELECT 1 FROM item_dependency_waits w
  WHERE w.item_id=NEW.id AND w.cycle=NEW.cycle AND w.state='waiting'
 ))
BEGIN
 SELECT RAISE(ABORT,'prerequisite items are not completed');
END;
