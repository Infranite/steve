-- 92: what the workflow statistics measure (#242). `sessions.first_tool_at` is when the session's first
-- tool call was recorded (how long a fresh session takes to start working); NULL until it makes one, and
-- for sessions from before (the statistics then read their first `tool_call` event).
-- Closing invalidations and pull request merges were only timeline lines; from now on they are
-- `invalidate` and `merge` item marks. The lines written before become marks once, flagged `backfill`,
-- each invalidation with the reason code of the text its caller wrote (see `steve_core::cycle::void`).
ALTER TABLE sessions ADD COLUMN first_tool_at INTEGER;
INSERT INTO item_marks (workspace_id, item_id, ts, kind, to_v, actor, detail, backfill)
  SELECT i.workspace_id, t.item_id, t.ts, 'invalidate',
    CASE
      WHEN substr(t.text, 6) = '计划里又有没做完的步骤' THEN 'plan'
      WHEN substr(t.text, 6) LIKE '有新的交付%' THEN 'redeliver'
      WHEN substr(t.text, 6) = '收到新的消息' THEN 'message'
      WHEN substr(t.text, 6) LIKE '冻结之后结果又变了%' THEN 'changed'
      WHEN substr(t.text, 6) = '工作图已调整，需要按最新结果统一验收' THEN 'graph'
      WHEN substr(t.text, 6) = '工作单元未通过' THEN 'unit_failed'
      WHEN substr(t.text, 6) = '验收没有通过，返工' THEN 'rework'
      WHEN substr(t.text, 6) = '你说它其实做完了，重新验收' THEN 'reaccept'
      WHEN substr(t.text, 6) = '你撤销了标记完成' THEN 'undone'
      ELSE 'other'
    END,
    'steve', json_object('why', substr(t.text, 6)), 1
  FROM timeline t JOIN items i ON i.id = t.item_id
  WHERE t.kind = 'sys' AND t.actor = 'steve' AND t.text LIKE '收尾作废：%';
INSERT INTO item_marks (workspace_id, item_id, ts, kind, to_v, actor, ref, detail, backfill)
  SELECT i.workspace_id, t.item_id, t.ts, 'merge', 'merged', 'steve',
    substr(t.text, instr(t.text, '：') + 1), '{}', 1
  FROM timeline t JOIN items i ON i.id = t.item_id
  WHERE t.kind = 'sys' AND t.actor = 'steve' AND t.text LIKE 'PR #% 已合并：%';
