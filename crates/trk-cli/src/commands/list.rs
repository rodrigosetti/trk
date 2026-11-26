use trk_core::error::Result;

use crate::Cmd;
use crate::env::Env;
use crate::render::Out;

use trk_core::error::{Code, Error};

pub(super) fn run(env: &Env, cmd: Cmd) -> Result<Out> {
    match cmd {
        Cmd::List {
            query,
            fields,
            sort,
            limit,
        } => list(env, &query.join(" "), fields, sort, limit, false),
        Cmd::Ready {
            query,
            fields,
            sort,
            limit,
        } => list(env, &query.join(" "), fields, sort, limit, true),
        _ => unreachable!(),
    }
}

fn list(
    env: &Env,
    q: &str,
    fields: Option<Vec<String>>,
    sort: Option<String>,
    limit: Option<usize>,
    ready: bool,
) -> Result<Out> {
    let mut q = q.trim().to_string();
    let mut fields = fields;
    let mut sort = sort;
    let mut limit = limit;
    if let Some(name) = q.strip_prefix('@') {
        let (name, rest) = name.split_once(' ').unwrap_or((name, ""));
        let v = env.user.views.get(name).ok_or_else(|| {
            Error::new(Code::NotFound, format!("no view '@{name}'")).with_hint(format!(
                "views: {}",
                env.user
                    .views
                    .keys()
                    .map(|k| format!("@{k}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
        })?;
        q = format!("{} {rest}", v.query);
        fields = fields.or_else(|| v.fields.clone());
        sort = sort.or_else(|| v.sort.clone());
        limit = limit.or(v.limit);
    }
    if ready {
        q = format!("ready {q}");
    }
    let store = env.store()?;
    let items = store.read(env.actor.as_deref(), |c| {
        c.list(q.trim(), sort.as_deref(), limit)
    })?;
    let items = items
        .into_iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<_, _>>()?;
    Ok(Out::Issues { items, fields })
}
