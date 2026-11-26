use serde_json::json;
use trk_core::error::Result;

use crate::EpicCmd;
use crate::env::Env;
use crate::render::{Out, Renderer};
use super::read_value;

pub(super) fn run(env: &Env, cmd: EpicCmd, r: &Renderer) -> Result<Out> {
    let mut store = env.store()?;
    if let EpicCmd::Ls { all } = cmd {
        let epics = store.read(None, |c| c.epics(all))?;
        return Ok(Out::Text(r.epics(&epics), json!(epics)));
    }
    let actor = env.actor()?.to_string();
    let epic = match cmd {
        EpicCmd::New { name, description } => {
            let description = description
                .as_deref()
                .map(read_value)
                .transpose()?
                .unwrap_or_default();
            store.write(&actor, false, |c| c.create_epic(&name, &description))?
        }
        EpicCmd::Rename { old, new } => {
            store.write(&actor, false, |c| c.rename_epic(&old, &new))?
        }
        EpicCmd::Archive { name } => store.write(&actor, false, |c| c.archive_epic(&name))?,
        EpicCmd::Ls { .. } => unreachable!(),
    };
    Ok(Out::Data(json!(epic)))
}
