use trk_core::error::Result;

use crate::Cmd;
use crate::env::Env;
use crate::render::Out;

use serde_json::json;
use super::{read_value, with_open};

pub(super) fn run(env: &Env, cmd: Cmd) -> Result<Out> {
    match cmd {
        Cmd::Note {
            id,
            text,
            amends,
            to_agent,
        } => {
            let actor = env.actor()?.to_string();
            let body = match text.as_deref() {
                None | Some("-") => read_value("@-")?,
                Some(t) => t.to_string(),
            };
            with_open(env, |s| {
                let seq = s.write(&actor, false, |c| c.note(&id, &body, amends, to_agent))?;
                Ok(Out::Text(
                    format!(
                        "note #{seq} added{}",
                        if to_agent {
                            " and queued for the agent"
                        } else {
                            ""
                        }
                    ),
                    json!({"seq": seq, "to_agent": to_agent}),
                ))
            })
        }
        Cmd::Log { id } => {
            let store = env.store()?;
            let (id, events) = store.read(None, |c| match id {
                Some(id) => {
                    let id = c.resolve_id(&id)?;
                    Ok((id.clone(), c.log(&id)?))
                }
                None => Ok(("All events".to_string(), c.all_events()?)),
            })?;
            Ok(Out::Log {
                issue: id,
                events: events
                    .into_iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect(),
            })
        }
        _ => unreachable!(),
    }
}

