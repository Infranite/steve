-- 134: remove legacy raw ACP event rows and backfill sessions.first_tool_at from steve.v1 tool events.
DELETE FROM events
 WHERE kind IN (
     'tool_call',
     'tool_call_update',
     'agent_message_chunk',
     'user_message_chunk',
     'agent_thought_chunk'
 )
    OR format != 'steve.v1';

UPDATE sessions
   SET first_tool_at = (
       SELECT MIN(e.ts)
         FROM events e
        WHERE e.session_id = sessions.id
          AND e.kind = 'tool'
   )
 WHERE first_tool_at IS NULL
   AND EXISTS (
       SELECT 1
         FROM events e
        WHERE e.session_id = sessions.id
          AND e.kind = 'tool'
   );
