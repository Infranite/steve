-- Keep only inverse operations that can be rebuilt from the recorded state.
-- Clearing an undo hint leaves the decision, its result and any choice receipt intact.
UPDATE decisions
   SET undo = CASE
       WHEN NOT json_valid(undo) THEN NULL
       WHEN json_extract(undo, '$.module') = subject_type
         AND subject_type IN ('chan_message','mail_draft','mem','party_merge','attr_value','attr_def','steward','space')
         THEN json_object('operation','domain')
       WHEN subject_type = 'mem' AND json_extract(undo,'$.auto') = 1
         AND json_valid(result) AND json_extract(result,'$.auto') = 'owner_said'
         THEN json_object('operation','domain')
       WHEN kind = 'accept' AND json_type(undo, '$.plan') = 'array'
         AND NOT EXISTS (
           SELECT 1 FROM json_each(undo, '$.plan') step
            WHERE CASE WHEN step.type <> 'object' THEN 1
                       ELSE coalesce(json_type(step.value,'$.step'),'null') <> 'text'
                         OR coalesce(json_type(step.value,'$.done'),'null') NOT IN ('true','false')
                         OR coalesce(json_type(step.value,'$.left'),'false') NOT IN ('true','false')
                  END
         )
         THEN json_object('operation','restore_plan','plan',json_extract(undo,'$.plan'))
       WHEN json_type(undo,'$.resume_at_revision') IN ('integer','null')
         THEN json_object('operation','resume','revision',json_extract(undo,'$.resume_at_revision'))
       ELSE NULL END
 WHERE undo IS NOT NULL;
