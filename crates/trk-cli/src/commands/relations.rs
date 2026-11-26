use trk_core::error::Result;

use crate::Cmd;
use crate::env::Env;
use crate::render::Out;

use serde_json::json;
use crate::DepCmd;
use super::with_open;

pub(super) fn run(env: &Env, cmd: Cmd) -> Result<Out> {
    match cmd {
        Cmd::Dep { cmd } => {
            let actor = env.actor()?.to_string();
            with_open(env, |s| match cmd {
                DepCmd::Add { blocker, blocked } => {
                    let (added, blocker, blocked) = s.write(&actor, false, |c| {
                        let (a, b) = (c.resolve_id(&blocker)?, c.resolve_id(&blocked)?);
                        Ok((c.dep_add(&a, &b)?, a, b))
                    })?;
                    Ok(Out::Text(
                        format!(
                            "{blocker} blocks {blocked}{}",
                            if added { "" } else { " (already present)" }
                        ),
                        json!({"blocker": blocker, "blocked": blocked, "added": added}),
                    ))
                }
                DepCmd::Rm { blocker, blocked } => {
                    let (blocker, blocked) = s.write(&actor, false, |c| {
                        let (a, b) = (c.resolve_id(&blocker)?, c.resolve_id(&blocked)?);
                        c.dep_rm(&a, &b)?;
                        Ok((a, b))
                    })?;
                    Ok(Out::Text(
                        format!("{blocker} no longer blocks {blocked}"),
                        json!({"blocker": blocker, "blocked": blocked}),
                    ))
                }
            })
        }
        Cmd::Link { id, url, label } => {
            let actor = env.actor()?.to_string();
            with_open(env, |s| {
                let (id, changed, links) = s.write(&actor, false, |c| {
                    let id = c.resolve_id(&id)?;
                    let changed = c.link_add(&id, &url, label.as_deref())?;
                    Ok((id.clone(), changed, c.links(&id)?))
                })?;
                Ok(Out::Text(
                    format!(
                        "linked {id} to {}{}",
                        url.trim(),
                        if changed { "" } else { " (already present)" }
                    ),
                    json!({"id": id, "changed": changed, "links": links}),
                ))
            })
        }
        Cmd::Unlink { id, url } => {
            let actor = env.actor()?.to_string();
            with_open(env, |s| {
                let (id, links) = s.write(&actor, false, |c| {
                    let id = c.resolve_id(&id)?;
                    c.link_rm(&id, &url)?;
                    Ok((id.clone(), c.links(&id)?))
                })?;
                Ok(Out::Text(
                    format!("unlinked {id} from {}", url.trim()),
                    json!({"id": id, "links": links}),
                ))
            })
        }
        _ => unreachable!(),
    }
}

