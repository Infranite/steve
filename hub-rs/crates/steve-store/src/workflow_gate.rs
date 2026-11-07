//! The persisted execution gate shared by session routers and Node queue planning. It accepts a
//! connection so the check and a Node's turn claim can use the same write transaction.

use rusqlite::{Connection, OptionalExtension};

/// Why an item or work-unit session cannot start another turn. Persisted pauses and prerequisite
/// waits apply to both. This is a state gate only: an execution runtime must separately establish
/// sandbox and principal isolation.
pub fn session_blocker(c: &Connection, session: &str) -> crate::Result<Option<String>> {
    Ok(c.query_row(
        "SELECT CASE
          WHEN i.status='pause' THEN '事项已暂停，恢复后再执行。'
          WHEN i.status='block' THEN '事项正在等待前置完成，不能开始执行。'
          WHEN EXISTS(SELECT 1 FROM item_dependency_waits dw WHERE dw.item_id=i.id AND dw.cycle=i.cycle AND dw.state='waiting')
            THEN '事项正在等待前置完成后续跑，不能开始执行。'
          WHEN EXISTS(SELECT 1 FROM item_deps d LEFT JOIN items dep ON dep.id=d.dep_id
            WHERE d.item_id=i.id AND (dep.id IS NULL OR dep.status<>'done' OR dep.phase<>'closed' OR dep.outcome IS NOT 'completed'))
            THEN '前置事项尚未完成，不能开始执行。'
          WHEN u.item_id IS NULL AND COALESCE(json_extract(i.source,'$[1]'),'')<>'workflow'
            AND NOT EXISTS(SELECT 1 FROM workflow_attempts a WHERE a.item_id=i.id) THEN NULL
          WHEN u.item_id IS NULL THEN '工作尝试已被替换，旧会话不能继续执行。'
          WHEN u.session_id IS NOT s.id OR s.status='ended'
            THEN '这不是工作单元当前的执行会话，不能继续执行。'
          WHEN u.status NOT IN ('dispatching','running')
            THEN '工作单元已经停止或交付，不能再启动回合。'
          WHEN w.item_id IS NULL OR p.id IS NULL THEN '主事项的工作图已不存在。'
          WHEN i.workspace_id<>s.workspace_id OR p.workspace_id<>s.workspace_id
            THEN '工作单元与主事项不属于同一工作空间。'
          WHEN w.cycle<>p.cycle OR w.goal_version<>
            COALESCE((SELECT MAX(g.version) FROM item_goals g WHERE g.item_id=p.id),0)
            THEN '主事项的目标或周期已变化，请重新规划后再执行。'
          WHEN w.paused=1 OR p.status='pause' THEN '协作工作已暂停，待主事项恢复后再执行。'
          WHEN p.status='block' OR EXISTS(SELECT 1 FROM item_dependency_waits dw WHERE dw.item_id=p.id AND dw.cycle=p.cycle AND dw.state='waiting')
            OR EXISTS(SELECT 1 FROM item_deps d LEFT JOIN items dep ON dep.id=d.dep_id
            WHERE d.item_id=p.id AND (dep.id IS NULL OR dep.status<>'done' OR dep.phase<>'closed' OR dep.outcome IS NOT 'completed'))
            THEN '主事项正在等待前置完成，不能开始执行。'
          WHEN p.phase='closed' OR p.archived=1 OR
            (p.phase='closing' AND COALESCE(json_extract(p.closing,'$.kind'),'')<>'complete')
            THEN '主事项已结束或正在停止，工作单元不能继续执行。'
          ELSE NULL END
         FROM sessions s JOIN items i ON i.id=s.item_id
         LEFT JOIN workflow_units u ON u.item_id=i.id
         LEFT JOIN workflows w ON w.item_id=u.workflow_id
         LEFT JOIN items p ON p.id=w.item_id WHERE s.id=?",
        [session],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten())
}
