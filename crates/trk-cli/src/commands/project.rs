use std::path::Path;

use serde_json::json;
use trk_core::error::Result;

use crate::env::Env;
use crate::render::Out;
use crate::skill;

pub(super) fn whoami(env: &Env) -> Result<Out> {
    let store = env.store()?;
    let (counts, lock) = store.read(None, |c| Ok((c.counts()?, c.check_lock()?)))?;
    let data = json!({
        "version": env!("TRK_VERSION"),
        "actor": env.actor,
        "root": env.root,
        "dir": env.dir,
        "user_dir": env.config.user_path,
        "db": env.db,
        "prefix": env.user.issues.prefix,
        "worktrees": env.worktrees_dir(),
        "skill": skill::configured_state(&env.root, &env.user.skill),
        "default_type": env.user.issues.default_type,
        "issues": counts,
        "check_running": lock.map(|(pid, host, started, pgid)| json!({"pid": pid, "host": host, "started_at": started, "pgid": pgid})),
    });
    Ok(Out::Data(data))
}

pub(super) fn backup(env: &Env, path: &Path) -> Result<Out> {
    let store = env.store()?;
    store.backup(path)?;
    Ok(Out::Text(
        format!("backup written to {}", path.display()),
        json!({"path": path}),
    ))
}
