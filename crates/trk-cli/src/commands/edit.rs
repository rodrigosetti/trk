use trk_core::error::Result;

use crate::Cmd;
use crate::env::Env;
use crate::render::Out;

use trk_core::error::bad_args;
use trk_core::ops::Edit;
use crate::editor;
use super::{SHOWN, read_value};

pub(super) fn run(env: &Env, cmd: Cmd) -> Result<Out> {
    match cmd {
        Cmd::Edit {
            id,
            title,
            description,
            priority,
            issue_type,
            epic,
            clear_epic,
            dry_run,
        } => edit(
            env,
            id,
            Edit {
                title,
                description: description.as_deref().map(read_value).transpose()?,
                priority,
                issue_type,
                epic: if clear_epic {
                    Some(None)
                } else {
                    epic.map(Some)
                },
            },
            dry_run,
        ),
        _ => unreachable!(),
    }
}

fn edit(env: &Env, id: String, e: Edit, dry_run: bool) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let nothing = e.title.is_none()
        && e.description.is_none()
        && e.priority.is_none()
        && e.issue_type.is_none()
        && e.epic.is_none();
    if !nothing {
        return store
            .write(&actor, dry_run, |c| {
                let id = c.resolve_id(&id)?;
                c.edit(&id, e)?;
                c.show(&id, SHOWN)
            })
            .map(Out::Issue);
    }
    if !editor::is_tty() {
        return Err(bad_args("nothing to change").with_hint(
            "pass -t TITLE, -d DESCRIPTION, -p PRIORITY, --type NAME, --epic NAME or --clear-epic",
        ));
    }
    let cur = store.read(None, |c| {
        let id = c.resolve_id(&id)?;
        c.issue(&id)
    })?;
    let doc = editor::Doc {
        title: cur.title.clone(),
        priority: cur.priority.clone(),
        issue_type: cur.issue_type.clone(),
        epic: cur.epic.clone().unwrap_or_default(),
        description: cur.description.clone(),
    };
    let v = editor::edit_loop(&doc, |d| {
        store.write(&actor, dry_run, |c| {
            c.edit(
                &cur.id,
                Edit {
                    title: Some(d.title),
                    description: Some(d.description),
                    priority: Some(d.priority),
                    issue_type: (d.issue_type.trim() != cur.issue_type).then_some(d.issue_type),
                    epic: {
                        let epic = (!d.epic.trim().is_empty()).then(|| d.epic.trim().to_string());
                        (epic != cur.epic).then_some(epic)
                    },
                },
            )?;
            c.show(&cur.id, SHOWN)
        })
    })?;
    Ok(Out::Issue(v))
}
