-- 117: data that older builds left in shapes the current code no longer reads goes, or is
-- rewritten once into the current shape; nothing reads the old shape any more.

-- An outbound reply owns its link to the inbound message, independent of provider reactions.
ALTER TABLE chan_messages ADD COLUMN answer_to TEXT REFERENCES chan_messages(id) ON DELETE SET NULL;
CREATE INDEX chan_messages_answer_to ON chan_messages(answer_to, workspace_id, created)
 WHERE answer_to IS NOT NULL;
UPDATE chan_messages AS outbound
   SET answer_to = inbound.id
  FROM messages AS answer
  JOIN messages AS turn ON turn.id = json_extract(answer.meta, '$.origin')
                       AND turn.workspace_id = answer.workspace_id
  JOIN chan_messages AS inbound ON inbound.id = json_extract(turn.meta, '$.chan.message')
                               AND inbound.workspace_id = turn.workspace_id
 WHERE answer.author = 'steve' AND turn.author = 'me'
   AND outbound.id = json_extract(answer.meta, '$.chan_out.message')
   AND outbound.workspace_id = inbound.workspace_id AND outbound.space_id = inbound.space_id
   AND outbound.direction = 'out' AND inbound.direction = 'in';
UPDATE chan_messages AS outbound
   SET answer_to = inbound.id
  FROM chan_reactions AS reaction JOIN chan_messages AS inbound ON inbound.id = reaction.message_id
 WHERE outbound.answer_to IS NULL AND outbound.id = reaction.answer_id
   AND outbound.workspace_id = inbound.workspace_id AND outbound.space_id = inbound.space_id
   AND outbound.direction = 'out' AND inbound.direction = 'in';

-- A Steve reply marked sent with no outbound message behind it was never confirmed by a provider.
UPDATE messages
   SET meta = json_set(meta, '$.chan_out.status', 'unknown', '$.chan_out.message', NULL, '$.chan_out.decision', NULL,
                       '$.chan_out.reason', '没有可核实的出站记录，发送结果未确认。')
 WHERE json_valid(meta) AND json_extract(meta, '$.chan_out.status') = 'sent'
   AND NOT EXISTS (SELECT 1 FROM chan_messages c
                    WHERE c.id = json_extract(messages.meta, '$.chan_out.message')
                      AND c.workspace_id = messages.workspace_id AND c.direction = 'out'
                      AND messages.space = 'chan:' || c.space_id);

-- An item card whose "released version" is a sentence was a failed build: it is the build line.
UPDATE messages
   SET meta = json_remove(json_set(meta, '$.card.build', json(
         CASE WHEN json_type(meta, '$.card.released.at') IN ('integer', 'real')
              THEN json_object('text', json_extract(meta, '$.card.released.version'),
                               'at', json_extract(meta, '$.card.released.at'))
              ELSE json_object('text', json_extract(meta, '$.card.released.version')) END)),
         '$.card.released')
 WHERE json_valid(meta) AND json_type(meta, '$.card.released.version') = 'text'
   AND json_type(meta, '$.card.build') IS NULL
   AND (instr(json_extract(meta, '$.card.released.version'), ' ') > 0
        OR instr(json_extract(meta, '$.card.released.version'), char(10)) > 0
        OR instr(json_extract(meta, '$.card.released.version'), char(9)) > 0);
UPDATE messages SET meta = json_remove(meta, '$.card.released')
 WHERE json_valid(meta) AND json_type(meta, '$.card.released.version') = 'text'
   AND (instr(json_extract(meta, '$.card.released.version'), ' ') > 0
        OR instr(json_extract(meta, '$.card.released.version'), char(10)) > 0
        OR instr(json_extract(meta, '$.card.released.version'), char(9)) > 0);

-- A Stop asked without the moment it was asked at cannot be told from a later turn's: it goes.
UPDATE session_stops SET stop_asked = NULL WHERE stop_asked IS NOT NULL AND asked_seq IS NULL;

-- Input queued on an ended session is never delivered.
DELETE FROM queued WHERE session_id IN (SELECT id FROM sessions WHERE status = 'ended');

-- A model choice stored before choices named their harness ran on the default adapter.
UPDATE settings SET value = json_set(value, '$.harness', 'claude-code')
 WHERE key LIKE 'models.%' AND key NOT LIKE 'models.agent.%' AND json_valid(value)
   AND json_type(value) = 'object' AND json_type(value, '$.harness') IS NULL
   AND (json_type(value, '$.models') = 'array' OR json_type(value, '$.effort') IS NOT NULL);

-- The environment's model lists are seeded once: one marker instead of two versioned ones.
INSERT OR IGNORE INTO settings (workspace_id, key, value, updated)
  SELECT workspace_id, 'models.seeded', 'true', MAX(updated) FROM settings
   WHERE key IN ('models.v2', 'models.v3') GROUP BY workspace_id;
DELETE FROM settings WHERE key IN ('models.v2', 'models.v3');

-- Open handoff cards from before they had a candidate draft offered `draft_reply`: they offer the
-- current choices without a draft.
UPDATE decisions SET options = '["instruct","verbatim","to_item","done"]'
 WHERE status = 'open' AND subject_type = 'chan_message' AND json_valid(options)
   AND EXISTS (SELECT 1 FROM json_each(decisions.options) WHERE value = 'draft_reply');

-- Open "new group" cards from before `quiet` and `block` existed offer all four choices.
UPDATE decisions SET options = '["open","mute","quiet","block"]'
 WHERE status = 'open' AND subject_type = 'space' AND json_valid(options)
   AND json_array_length(options) = 2
   AND EXISTS (SELECT 1 FROM json_each(decisions.options) WHERE value = 'open')
   AND EXISTS (SELECT 1 FROM json_each(decisions.options) WHERE value = 'mute');

-- A tool pinned by an operation's old name is pinned by its family; usage counted under the old
-- names goes.
INSERT OR IGNORE INTO bot_tool_pins (bot_id, kind, target, created)
  SELECT bot_id, 'tool', CASE target
           WHEN 'steve_contacts_find' THEN 'steve_contacts'
           WHEN 'steve_whois' THEN 'steve_contacts'
           WHEN 'steve_person_holdings' THEN 'steve_contacts'
           WHEN 'steve_github_status' THEN 'steve_github_read'
           WHEN 'steve_github_ci_log' THEN 'steve_github_read'
           WHEN 'steve_mail_thread' THEN 'steve_mail_read'
           WHEN 'steve_mail_attachment' THEN 'steve_mail_read'
           WHEN 'steve_matters_search' THEN 'steve_matters'
           WHEN 'steve_matter_read' THEN 'steve_matters' END, MIN(created)
    FROM bot_tool_pins
   WHERE kind = 'tool' AND target IN ('steve_contacts_find', 'steve_whois', 'steve_person_holdings',
     'steve_github_status', 'steve_github_ci_log', 'steve_mail_thread', 'steve_mail_attachment',
     'steve_matters_search', 'steve_matter_read')
   GROUP BY 1, 2, 3;
DELETE FROM bot_tool_pins
 WHERE kind = 'tool' AND target IN ('steve_contacts_find', 'steve_whois', 'steve_person_holdings',
   'steve_github_status', 'steve_github_ci_log', 'steve_mail_thread', 'steve_mail_attachment',
   'steve_matters_search', 'steve_matter_read');
DELETE FROM tool_usage
 WHERE tool IN ('steve_contacts_find', 'steve_whois', 'steve_person_holdings', 'steve_github_status',
   'steve_github_ci_log', 'steve_mail_thread', 'steve_mail_attachment', 'steve_matters_search',
   'steve_matter_read');

-- Billing companions used to be inferred at read time. Preserve cost without reporting a
-- second token attempt when the recorded turn or call identifies the same accounting event.
UPDATE usage_records AS u
   SET detail = json_set(u.detail, '$.accounting_tokens', json('false'))
 WHERE json_valid(u.detail) AND json_type(u.detail, '$.accounting_tokens') IS NULL
   AND json_extract(u.detail, '$.scope') = 'main' AND u.cost_reported IS NOT NULL
   AND u.input IS NULL AND u.output IS NULL AND u.cache_read IS NULL AND u.cache_write IS NULL
   AND EXISTS (
       SELECT 1 FROM usage_records p
        WHERE p.workspace_id = u.workspace_id AND p.day = u.day AND p.source = u.source
          AND p.purpose = u.purpose AND p.harness = u.harness AND p.model = u.model
          AND p.item_id IS u.item_id AND p.session_id IS u.session_id
          AND p.bot_id IS u.bot_id AND p.project_id IS u.project_id
          AND json_valid(p.detail) AND json_extract(p.detail, '$.scope') = 'all'
          AND (p.input IS NOT NULL OR p.output IS NOT NULL OR p.cache_read IS NOT NULL OR p.cache_write IS NOT NULL)
          AND (p.turn_id = u.turn_id OR
               NULLIF(json_extract(p.detail, '$.call_id'), '') = NULLIF(json_extract(u.detail, '$.call_id'), '')));

-- A space is read through its recorded identities. Fill the first sighting where it is missing.
INSERT INTO space_identities(space_id, connection_id, workspace_id, joined, left_at, ordinal, owner_user, platform)
 SELECT s.id, cn.id, s.workspace_id, s.created,
        CASE WHEN s.status = 'active' THEN NULL ELSE s.updated END, 0, p.user_id,
        CASE WHEN json_valid(cn.config) THEN json_extract(cn.config, '$.platform') END
 FROM spaces s JOIN connections cn ON cn.id = s.connection_id AND cn.workspace_id = s.workspace_id
 LEFT JOIN accounts a ON a.id = cn.account_id
 LEFT JOIN parties p ON p.id = a.party_id
 WHERE NOT EXISTS (SELECT 1 FROM space_identities i WHERE i.space_id = s.id);

-- Undo reads the space flags as recorded at the time of the decision.
UPDATE decisions SET result = json_set(result, '$.was.attention', 'realtime')
 WHERE subject_type = 'space' AND json_valid(result)
   AND json_type(result, '$.was') = 'object' AND json_type(result, '$.was.attention') IS NULL;

-- Completion events without a cycle/revision cannot safely trigger work again.
CREATE TEMP TABLE incomplete_completion AS
 SELECT seq FROM outbox_events WHERE topic = 'work.item.done' AND
  CASE WHEN json_valid(payload) THEN
   json_type(payload, '$.cycle') IS NOT 'integer' OR json_type(payload, '$.revision') IS NOT 'integer'
  ELSE 1 END;
DELETE FROM outbox_receipts WHERE event_seq IN (SELECT seq FROM incomplete_completion);
DELETE FROM outbox_leases WHERE event_seq IN (SELECT seq FROM incomplete_completion);
DELETE FROM outbox_events WHERE seq IN (SELECT seq FROM incomplete_completion);
DROP TABLE incomplete_completion;
