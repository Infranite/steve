-- Explicit owner completion is final even when prerequisites were left open.
DROP TRIGGER item_dependencies_completion_gate;
CREATE TRIGGER item_dependencies_completion_gate
BEFORE UPDATE OF phase,outcome,status,cycle ON items
WHEN NEW.phase='closed' AND NEW.outcome='completed' AND NEW.acceptance IS NOT 'owner'
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
