use super::*;

pub fn seed_grant_gr_1(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created)
         VALUES ('gr_1', 'default', 'flow', 'flw_x', 'msg.send', 'space', '*', json_object('flow_version', 1), 99999999999999, 'usr_dev', 1);"#)?;
        Ok(())
    })
}

pub fn seed_grant_gr_1_for_an_approved_flow_step_is_the_only_a(
    store: &Store,
    plan_space_id: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created)
             VALUES ('gr_1', 'default', 'flow', 'flw_test', 'msg.send', 'space', ?1, json_object('flow_version', 1), 99999999999999, 'usr_dev', 1);"#, params![plan_space_id])?;
        Ok(())
    })
}

pub fn seed_grant_gr_2(store: &Store, mine_space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created)
             VALUES ('gr_2', 'default', 'flow', 'flw_test', 'msg.send', 'space', ?1, json_object('flow_version', 1), 99999999999999, 'usr_dev', 1);"#, params![mine_space_id])?;
        Ok(())
    })
}

pub fn seed_grant_gr_old(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created, revoked)
         VALUES ('gr_old', 'default', 'flow', 'flw_old', 'msg.send', 'space', '*', json_object('flow_version', 1), 2, 'usr_dev', 1, NULL),
                ('gr_rev', 'default', 'flow', 'flw_rev', 'msg.send', 'space', '*', json_object('flow_version', 1), 99999999999999, 'usr_dev', 1, 5);"#)?;
        Ok(())
    })
}

pub fn seed_grant_gr_me(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created)
         VALUES ('gr_me', 'default', 'flow', 'flw_me', 'msg.send', 'space', '*', json_object('flow_version', 1), 99999999999999, 'usr_dev', 1);"#)?;
        Ok(())
    })
}
