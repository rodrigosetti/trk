use trk_core::error::Result;

use crate::Cmd;
use crate::env::Env;
use crate::render::Out;

use super::{SHOWN, read_value, show_one, with_open};
use trk_core::model::OnPass;
use trk_core::ops::WaitOpts;

pub(super) fn run(env: &Env, cmd: Cmd) -> Result<Out> {
    match cmd {
        Cmd::Close {
            id,
            reason,
            message,
            force,
            dry_run,
        } => {
            let actor = env.actor()?.to_string();
            with_open(env, |s| {
                s.write(&actor, dry_run, |c| {
                    c.close(&id, &reason, message.as_deref(), force)?;
                    c.show(&id, SHOWN)
                })
                .map(Out::Issue)
            })
        }
        Cmd::Wait {
            id,
            check,
            on_pass,
            every,
            reason,
            force,
            dry_run,
        } => {
            let actor = env.actor()?.to_string();
            let opts = WaitOpts {
                check: check.as_deref().map(read_value).transpose()?,
                on_pass: match on_pass.as_str() {
                    "wake" => OnPass::Wake,
                    "close" => OnPass::Close,
                    _ => unreachable!("clap validates --on-pass"),
                },
                interval: every
                    .map(|e| trk_core::config::parse_duration(&e))
                    .transpose()?,
                reason,
                force,
            };
            with_open(env, |s| {
                s.write(&actor, dry_run, |c| {
                    c.wait(&id, opts)?;
                    c.show(&id, SHOWN)
                })
                .map(Out::Issue)
            })
        }
        Cmd::Reopen { id, message } => {
            let actor = env.actor()?.to_string();
            with_open(env, |s| {
                s.write(&actor, false, |c| c.reopen(&id, message.as_deref()))?;
                show_one(env, s, &id)
            })
        }
        _ => unreachable!(),
    }
}
