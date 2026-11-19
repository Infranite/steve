use super::*;

install! { install(reg, cx) [mut jobs] => [jobs] {
    let HubEnv { cfg, .. } = *cx;
    // the daily copy of the database and what it cannot bring back; a Hub on a copy of a real
    // database (NO_RESUME) is not the one whose data is kept
    if !cfg.no_resume {
        jobs.push(Job::new("backup.daily", crate::daily_backup::run(
            cfg.home.clone(),
            cfg.db_path(),
            cfg.backup,
        )));
    }
}}
