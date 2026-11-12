//! The persisted execution gate shared by session routers and Node queue planning. It accepts a
//! connection so the check and a Node's turn claim can use the same write transaction.

use rusqlite::{Connection, OptionalExtension};

/// Why an item or work-unit session cannot start another turn. Persisted pauses and prerequisite
/// waits apply to both. This is a state gate only: an execution runtime must separately establish
/// the ordinary Node authorization and execution policy. Workflow identity is checked here too,
/// in the transaction that claims a queued turn, so recovery cannot bypass it.
pub fn session_blocker(c: &Connection, session: &str) -> crate::Result<Option<String>> {
    let blocked = c.query_row(
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
    .flatten();
    if blocked.is_some() {
        return Ok(blocked);
    }
    session_authority_blocker(c, session)
}

/// Authority of a workflow child, without its scheduling state. Ordinary coordinator sessions
/// stay usable to diagnose and repair a graph that cannot currently execute.
pub fn session_authority_blocker(c: &Connection, session: &str) -> crate::Result<Option<String>> {
    let item: Option<String> = c
        .query_row(
            "SELECT i.id FROM sessions s JOIN items i ON i.id=s.item_id WHERE s.id=?
           AND (json_extract(i.source,'$[1]')='workflow'
             OR EXISTS(SELECT 1 FROM workflow_units WHERE item_id=i.id)
             OR EXISTS(SELECT 1 FROM workflow_attempts WHERE item_id=i.id))",
            [session],
            |r| r.get(0),
        )
        .optional()?;
    match item {
        Some(item) => item_blocker(c, &item),
        None => Ok(None),
    }
}

/// Positive same-principal admission for a graph or its current child. NULL `requested_by` is
/// the workspace's explicit service principal, as for ordinary items; an unknown/departed member
/// is refused here rather than silently changing a collaborative run to that service principal.
/// This check is independent of pause/state checks (a paused graph can still be authorized).
pub fn item_blocker(c: &Connection, item: &str) -> crate::Result<Option<String>> {
    let parent: Option<String> = c
        .query_row(
            "SELECT workflow_id FROM workflow_units WHERE item_id=?",
            [item],
            |r| r.get(0),
        )
        .optional()?;
    let graph = match parent {
        Some(parent) => parent,
        None => {
            let retired: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM items WHERE id=?1 AND json_extract(source,'$[1]')='workflow')
                 OR EXISTS(SELECT 1 FROM workflow_attempts WHERE item_id=?1)", [item], |r| r.get(0),
            )?;
            if retired {
                return Ok(Some("工作尝试已被替换，旧会话不能继续执行。".into()));
            }
            let graph: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflows WHERE item_id=?)",
                [item],
                |r| r.get(0),
            )?;
            if !graph {
                return Ok(None);
            }
            item.to_string()
        }
    };
    // Check the raw requester as well as its resolved identity: two unknown requesters must not
    // become an accidental same-service-principal authorization. Existing live bindings must
    // agree too; credentials and harness state were selected when they were bound.
    let why: Option<String> = c.query_row(
        "SELECT CASE
          WHEN NOT EXISTS(SELECT 1 FROM workspaces WHERE id=p.workspace_id)
            OR (p.requested_by IS NOT NULL AND NOT EXISTS(SELECT 1 FROM workspace_members
                WHERE workspace_id=p.workspace_id AND user_id=p.requested_by))
            THEN '请求主体身份未知或已失效，不能执行协作工作。'
          WHEN EXISTS(SELECT 1 FROM sessions s JOIN runtime_sessions r ON r.session_id=s.id
            WHERE s.item_id=p.id AND s.status<>'ended'
              AND (s.workspace_id<>p.workspace_id OR r.principal IS NOT COALESCE(p.requested_by,'svc:'||p.workspace_id)))
            THEN '主事项的会话主体与当前请求主体不一致。'
          WHEN p.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects pr WHERE pr.id=p.project_id
            AND pr.workspace_id=p.workspace_id AND pr.archived=0 AND pr.kind='git' AND pr.worktree=1)
            THEN '协作项目需要同工作空间内可用的独立 Git worktree。'
          WHEN EXISTS(SELECT 1 FROM workflow_units u LEFT JOIN bots b ON b.id=json_extract(u.spec,'$.bot_id')
            WHERE u.workflow_id=p.id AND (?2=p.id OR u.item_id=?2)
              AND (b.id IS NULL OR b.enabled=0 OR b.deleted=1 OR NOT EXISTS(SELECT 1 FROM bot_workspaces bw
                WHERE bw.bot_id=b.id AND bw.workspace_id=p.workspace_id)))
            THEN '协作 Bot 不可用或不属于当前工作空间。'
          WHEN EXISTS(SELECT 1 FROM workflow_units u LEFT JOIN items ch ON ch.id=u.item_id
            WHERE u.workflow_id=p.id AND u.item_id IS NOT NULL AND (?2=p.id OR u.item_id=?2)
              AND (ch.id IS NULL OR ch.workspace_id<>p.workspace_id OR ch.requested_by IS NOT p.requested_by
                OR ch.project_id IS NOT p.project_id OR ch.bot_id IS NOT json_extract(u.spec,'$.bot_id')))
            THEN '工作单元与主事项的请求主体、工作空间、项目或 Bot 不一致。'
          WHEN EXISTS(SELECT 1 FROM workflow_units u JOIN sessions s ON s.item_id=u.item_id
            LEFT JOIN runtime_sessions r ON r.session_id=s.id
            WHERE u.workflow_id=p.id AND (?2=p.id OR u.item_id=?2) AND s.status<>'ended'
              AND (s.workspace_id<>p.workspace_id OR s.bot_id IS NOT json_extract(u.spec,'$.bot_id')
                OR (r.session_id IS NOT NULL AND r.principal IS NOT COALESCE(p.requested_by,'svc:'||p.workspace_id))))
            THEN '工作单元的会话主体、Bot 或工作空间与主事项不一致。'
          ELSE NULL END FROM items p WHERE p.id=?1",
        rusqlite::params![graph, item], |r| r.get(0),
    ).optional()?.unwrap_or_else(|| Some("主事项已不存在，无法确认请求主体。".into()));
    Ok(why)
}
