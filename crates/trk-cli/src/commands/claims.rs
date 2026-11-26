use trk_core::error::Result;

use crate::Cmd;
use crate::env::Env;
use crate::render::Out;

use super::{show_one, with_open};
use trk_core::ops::ClaimOpts;

pub(super) fn run(env: &Env, cmd: Cmd) -> Result<Out> {
    match cmd {
        Cmd::Claim {
            id,
            lease,
            worktree,
            steal,
        } => {
            let actor = env.actor()?.to_string();
            let lease = lease
                .map(|l| trk_core::config::parse_duration(&l))
                .transpose()?;
            let explicit = worktree.map(|p| {
                std::fs::canonicalize(&p)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string()
            });
            with_open(env, |s| {
                s.write(&actor, false, |c| {
                    let id = c.resolve_id(&id)?;
                    // Record where the work happens: the given worktree, or the
                    // working directory when it is a checkout of this project.
                    let worktree =
                        explicit.or_else(|| env.here().map(|p| p.to_string_lossy().to_string()));
                    c.claim(
                        &id,
                        ClaimOpts {
                            lease,
                            steal,
                            worktree,
                            host: trk_core::system::hostname().map(str::to_owned),
                            ..Default::default()
                        },
                    )
                    .map(|_| ())
                })?;
                show_one(env, s, &id)
            })
        }
        Cmd::Unclaim { id, force, message } => {
            let actor = env.actor()?.to_string();
            with_open(env, |s| {
                s.write(&actor, false, |c| c.unclaim(&id, force, message.as_deref()))?;
                show_one(env, s, &id)
            })
        }
        Cmd::Renew { id, lease } => {
            let actor = env.actor()?.to_string();
            let lease = lease
                .map(|l| trk_core::config::parse_duration(&l))
                .transpose()?;
            with_open(env, |s| {
                let c = s.write(&actor, false, |c| c.renew(&id, lease))?;
                Ok(Out::Text(
                    format!(
                        "{} lease until {}",
                        c.issue_id,
                        c.lease_until.clone().unwrap_or_else(|| "(no lease)".into())
                    ),
                    serde_json::to_value(&c)?,
                ))
            })
        }
        _ => unreachable!(),
    }
}
