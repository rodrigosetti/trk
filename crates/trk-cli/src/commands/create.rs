use trk_core::error::Result;

use crate::Cmd;
use crate::env::Env;
use crate::render::Out;

use super::{SHOWN, read_value};
use crate::{editor, run};
use serde_json::Value;
use trk_core::error::Error;
use trk_core::model::DEFAULT_PRIORITY;
use trk_core::ops::NewIssue;

pub(super) fn run(env: &Env, cmd: Cmd) -> Result<Out> {
    match cmd {
        Cmd::Create {
            title_arg,
            title,
            description,
            priority,
            issue_type,
            epic,
            blocked_by,
            blocks,
            id,
            run,
            dry_run,
        } => create(
            env,
            NewIssue {
                id,
                title: title.or(title_arg).unwrap_or_default(),
                description: description
                    .as_deref()
                    .map(read_value)
                    .transpose()?
                    .unwrap_or_default(),
                priority,
                issue_type,
                epic,
                blocked_by,
                blocks,
                ..Default::default()
            },
            dry_run,
            run,
        ),
        _ => unreachable!(),
    }
}

fn create(env: &Env, new: NewIssue, dry_run: bool, run: bool) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let mut attempt = |new: NewIssue| -> Result<Value> {
        store.write(&actor, dry_run, |c| {
            let id = c.create(new)?;
            c.show(&id, SHOWN)
        })
    };
    let v = if new.title.trim().is_empty() && editor::is_tty() && !dry_run {
        let doc = editor::Doc {
            title: String::new(),
            priority: new
                .priority
                .clone()
                .unwrap_or_else(|| DEFAULT_PRIORITY.to_string()),
            issue_type: new
                .issue_type
                .clone()
                .unwrap_or_else(|| env.user.issues.default_type.clone()),
            epic: new.epic.clone().unwrap_or_default(),
            description: if new.description.is_empty() {
                "<!-- What is needed and why, with enough context to start cold. -->\n".into()
            } else {
                new.description.clone()
            },
        };
        editor::edit_loop(&doc, |d| {
            let description = d.description.trim();
            let placeholder = description.starts_with("<!--") && description.ends_with("-->");
            attempt(NewIssue {
                title: d.title,
                priority: Some(d.priority).filter(|p| !p.trim().is_empty()),
                issue_type: Some(d.issue_type),
                epic: (!d.epic.trim().is_empty()).then(|| d.epic.trim().to_string()),
                description: if placeholder {
                    String::new()
                } else {
                    d.description
                },
                ..new.clone()
            })
        })?
    } else {
        attempt(new)?
    };
    if run {
        let id = v["id"].as_str().unwrap_or_default();
        return run::run(
            env,
            run::RunArgs {
                id: Some(id.to_string()),
                agent: None,
                harness: None,
                model: None,
                fg: false,
                print: false,
                dry_run: false,
            },
        )
        .map_err(|err| {
            let err = match err {
                Error::Coded(_) => err,
                other => Error::new(other.code(), other.to_string()),
            };
            err.with_issue(id)
        });
    }
    Ok(Out::Issue(v))
}
