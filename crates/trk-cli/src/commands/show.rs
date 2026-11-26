use trk_core::error::Result;

use crate::Cmd;
use crate::env::Env;
use crate::render::Out;

use serde_json::json;
use trk_core::view::With;

pub(super) fn run(env: &Env, cmd: Cmd) -> Result<Out> {
    match cmd {
        Cmd::Show { ids, with } => {
            let store = env.store()?;
            let w = With::parse(&with);
            let mut items = vec![];
            for id in &ids {
                let mut issue = store.read(env.actor.as_deref(), |c| c.show(id, w))?;
                if store
                    .opts
                    .known_types
                    .iter()
                    .any(|name| Some(name.as_str()) == issue["type"].as_str())
                {
                    let cfg = trk_agent::routing::RoutingConfig::new(&env.config);
                    issue["routing"] =
                        json!(cfg.resolve(issue["type"].as_str().unwrap_or_default(), None)?);
                }
                items.push(issue);
            }
            if items.len() == 1 {
                Ok(Out::Issue(items.pop().unwrap()))
            } else {
                Ok(Out::IssueList(items))
            }
        }
        _ => unreachable!(),
    }
}
