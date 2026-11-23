-- 116: one dispatch model. A session of an item runs on its Bot's ordered execution Nodes (the Hub's
-- own Node until configured), narrowed by the item's Team. The automatic spread of the Nodes paired
-- before Bot bindings existed goes with the router it served.
DROP TABLE legacy_spread_nodes;
-- A placement made by that router has no decision of its own: a recorded decision older than the
-- item's newest bound session describes something else, and goes.
UPDATE item_dispatch SET last_decision = NULL
 WHERE last_decision IS NOT NULL AND EXISTS (
   SELECT 1 FROM sessions s JOIN runtime_sessions r ON r.session_id = s.id
    WHERE s.item_id = item_dispatch.item_id
      AND s.id IS NOT json_extract(item_dispatch.last_decision, '$.session')
      AND s.created > COALESCE(json_extract(item_dispatch.last_decision, '$.at'), 0));
DELETE FROM item_dispatch WHERE team IS NULL AND last_decision IS NULL;
