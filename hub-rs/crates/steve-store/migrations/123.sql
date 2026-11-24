-- Decisions retain their ID and historical outcome. Each new answer is tied to the
-- version the caller saw, and reopening a card leaves its previous receipt intact.
ALTER TABLE decisions ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;

CREATE TABLE decision_choice_receipts (
    decision_id TEXT NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    answer TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (decision_id, revision)
);

-- Retire cards that cannot apply to the current domain. Leave their links and
-- every recorded choice in place; neither case authorizes a new action.
UPDATE decisions AS old
   SET status = 'superseded', resolved = created
 WHERE status = 'open'
   AND (
     subject_type IN ('delivery_batch','mq_target','mq_batch','mq_train')
     OR (
       kind IN ('ask','fail') AND subject_type IN ('question','item','stall')
       AND EXISTS (
         SELECT 1 FROM decisions AS later
          WHERE later.workspace_id = old.workspace_id
            AND later.item_id = old.item_id AND later.session_id = old.session_id
            AND later.kind IN ('accept','approve') AND later.created > old.created
       )
     )
   );

-- Old permission and stall rows already record their execution identity.
ALTER TABLE decisions ADD COLUMN source_revision TEXT;
UPDATE decisions SET source_revision = (
 SELECT json_object('kind','permission','node',p.node_id,'attempt',p.attempt_id)
 FROM runtime_permissions p WHERE p.decision_id=decisions.id
) WHERE subject_type='tool_call';
UPDATE decisions SET source_revision = (
 SELECT json_object('kind','turn_stall','attempt',p.stalled_attempt)
 FROM runtime_turn_progress p WHERE p.decision_id=decisions.id
) WHERE subject_type='turn_stall';
CREATE UNIQUE INDEX decisions_source_revision
 ON decisions(workspace_id,subject_type,subject_id,kind,source_revision);

CREATE INDEX runtime_permissions_expiry ON runtime_permissions(expires_at, decision_id)
 WHERE state = 'waiting';

ALTER TABLE decisions ADD COLUMN question_template TEXT
 CHECK (question_template IS NULL OR (question = '' AND json_valid(question_template)));

-- These prompts can be identified by their subject and exact machine-authored text.
UPDATE decisions
   SET question_template = '{"code":"turn_repeated_failure"}', question = ''
 WHERE subject_type = 'turn_stall'
   AND question = '同一工具操作连续累计失败五次，Steve 已请求停止。';
UPDATE decisions
   SET question_template = '{"code":"turn_no_progress"}', question = ''
 WHERE subject_type = 'turn_stall'
   AND question = '可观察的执行时间累计十五分钟仍无可核实的进展，Steve 已请求停止。';
UPDATE decisions
   SET question_template = json_object('code','merge_pull_request','number',pr.number,'repo',repo.full_name),
       question = ''
  FROM gh_prs pr JOIN gh_repos repo ON repo.id = pr.repo_id
 WHERE decisions.subject_type = 'gh_pr_merge'
   AND decisions.subject_id = pr.id AND decisions.workspace_id = pr.workspace_id
   AND (question = printf('合并 PR #%d（%s）？',pr.number,repo.full_name)
        OR question = printf('Merge PR #%d (%s)?',pr.number,repo.full_name));
