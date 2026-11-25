use super::*;

install! { install(reg, cx) [db, mut jobs] => [db, jobs] {
    let HubEnv { cfg, .. } = *cx;
    // the daily copy of the database and what it cannot bring back; a Hub on a copy of a real
    // database (NO_RESUME) is not the one whose data is kept
    if !cfg.diagnostic() {
        let home = cfg.home.clone();
        let db_path = cfg.db_path();
        let store = db.clone();
        let on = cfg.backup;
        jobs.push(Job::task("backup.daily", move || {
            crate::daily_backup::task(home, db_path, store, on)
        }));
    }
}}
