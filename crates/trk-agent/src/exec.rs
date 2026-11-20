//! Executing trk tools against the store. Every call is one transaction.

use serde_json::{Map, Value, json};
use trk_core::error::{Code, Error, Result, bad_args};
use trk_core::fields::{Assign, AssignOp};
use trk_core::ops::NewDep;
use trk_core::ops::NewIssue;
use trk_core::view::With;
use trk_core::{Ctx, Store};

use crate::tools::{Action, ToolDef};

/// The issue and actor a session serves.
#[derive(Debug, Clone)]
pub struct Session {
    pub issue: String,
    pub actor: String,
}

fn args_obj(args: &Value) -> Map<String, Value> {
    args.as_object().cloned().unwrap_or_default()
}

fn str_arg<'a>(a: &'a Map<String, Value>, k: &str) -> Result<&'a str> {
    a.get(k)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| bad_args(format!("missing required argument '{k}'")))
}

fn assigns_from(a: &Map<String, Value>, fields: &[String]) -> Vec<Assign> {
    fields
        .iter()
        .filter_map(|f| a.get(f).map(|v| Assign::set(f.clone(), v.clone())))
        .collect()
}

/// A compact view of the issue returned after each tool call.
pub fn compact(c: &Ctx, issue: &str) -> Result<Value> {
    let full = c.show(
        issue,
        With {
            deps: false,
            ..Default::default()
        },
    )?;
    Ok(json!({
        "id": full["id"],
        "status": full["status"],
        "status_category": full["status_category"],
        "resolution": full["resolution"],
        "claimed": !full["claim"].is_null(),
        "next": full["derived"]["next"],
    }))
}

pub fn call(store: &mut Store, sess: &Session, tool: &ToolDef, args: &Value) -> Result<Value> {
    let a = args_obj(args);
    let id = sess.issue.clone();
    let result: Value = match &tool.action {
        Action::GetTask => {
            return store.read(Some(&sess.actor), |c| {
                c.show(
                    &id,
                    With {
                        deps: true,
                        notes: true,
                        ..Default::default()
                    },
                )
            });
        }
        Action::AddNote => {
            let body = str_arg(&a, "body")?.to_string();
            let seq = store.write(&sess.actor, false, |c| c.note(&id, &body, None, false))?;
            json!({"note": seq})
        }
        Action::AddLink => {
            let url = str_arg(&a, "url")?;
            let mut link = json!({"kind": a.get("kind").and_then(|v| v.as_str()).unwrap_or("link"), "url": url});
            if let Some(n) = a.get("note").and_then(|v| v.as_str()) {
                link["note"] = json!(n);
            }
            let assign = Assign {
                field: "links".into(),
                op: AssignOp::Append,
                value: link,
            };
            store.write(&sess.actor, false, |c| c.edit(&id, &[assign]))?;
            json!({"linked": url})
        }
        Action::ReportFollowup => {
            let ty = str_arg(&a, "type")?.to_string();
            let mut assigns = vec![
                Assign::set("title", str_arg(&a, "title")?),
                Assign::set("context", str_arg(&a, "context")?),
            ];
            if let Some(Value::Object(f)) = a.get("fields") {
                for (k, v) in f {
                    assigns.push(Assign::set(k.clone(), v.clone()));
                }
            }
            let new_id = store.write(&sess.actor, false, |c| {
                let project = c.issue(&id)?.project;
                let mut assigns = assigns;
                inherit(c, &id, &ty, &mut assigns)?;
                c.create(NewIssue {
                    project,
                    ty,
                    assigns,
                    deps: vec![NewDep {
                        kind: "discovered-from".into(),
                        other: id.clone(),
                        new_is_src: true,
                    }],
                })
            })?;
            json!({"created": new_id})
        }
        Action::HandBack => {
            let note = str_arg(&a, "note")?.to_string();
            store.write(&sess.actor, false, |c| c.unclaim(&id, false, Some(&note)))?;
            return Ok(
                json!({"handed_back": id, "message": "Your claim is released. Stop working on this issue."}),
            );
        }
        Action::Move { status, fields } => {
            let assigns = assigns_from(&a, fields);
            let note = a.get("note").and_then(|v| v.as_str()).map(String::from);
            store.write(&sess.actor, false, |c| {
                c.move_to(&id, status, &assigns, false)?;
                if let Some(n) = &note {
                    c.note(&id, n, None, false)?;
                }
                Ok(())
            })?;
            json!({"moved_to": status})
        }
        Action::Close {
            resolution,
            fields,
            edges,
        } => {
            let assigns = assigns_from(&a, fields);
            let note = a.get("note").and_then(|v| v.as_str()).map(String::from);
            let mut targets = vec![];
            for e in edges {
                targets.push((e.clone(), str_arg(&a, &e.replace('-', "_"))?.to_string()));
            }
            store.write(&sess.actor, false, |c| {
                for (kind, dst) in &targets {
                    c.dep_add(&id, kind, dst)?;
                }
                if let Some(n) = &note {
                    c.note(&id, n, None, false)?;
                }
                c.close(&id, resolution, &assigns, false)
            })?;
            return Ok(json!({
                "closed": id, "resolution": resolution,
                "message": "The issue is closed and your claim released. Your work on it is done.",
            }));
        }
        Action::SetField { field } => {
            let v = a
                .get("value")
                .cloned()
                .ok_or_else(|| bad_args("missing required argument 'value'"))?;
            let changes = store.write(&sess.actor, false, |c| {
                c.edit(&id, &[Assign::set(field.clone(), v)])
            })?;
            json!({"changed": changes})
        }
        Action::Composed { spec } => {
            let note = if spec.note {
                a.get("note").and_then(|v| v.as_str()).map(String::from)
            } else {
                None
            };
            let set = assigns_from(&a, &spec.set);
            let out = store.write(&sess.actor, false, |c| {
                let mut out = json!({});
                if !set.is_empty() {
                    c.edit(&id, &set)?;
                }
                if let Some((ty, edge, fields, new_is_src)) = &spec.create {
                    let project = c.issue(&id)?.project;
                    let mut deps = vec![NewDep {
                        kind: edge.clone(),
                        other: id.clone(),
                        new_is_src: *new_is_src,
                    }];
                    if edge != "discovered-from" || !*new_is_src {
                        deps.push(NewDep {
                            kind: "discovered-from".into(),
                            other: id.clone(),
                            new_is_src: true,
                        });
                    }
                    let mut assigns = assigns_from(&a, fields);
                    inherit(c, &id, ty, &mut assigns)?;
                    let new_id = c.create(NewIssue {
                        project,
                        ty: ty.clone(),
                        assigns,
                        deps,
                    })?;
                    out["created"] = json!(new_id);
                }
                if let Some(to) = &spec.move_to {
                    c.move_to(&id, to, &rest_assigns(c, &a, spec), false)?;
                    out["moved_to"] = json!(to);
                }
                if let Some(n) = &note {
                    c.note(&id, n, None, false)?;
                }
                if let Some(r) = &spec.close {
                    c.close(&id, r, &rest_assigns(c, &a, spec), false)?;
                    out["closed"] = json!(r);
                }
                Ok(out)
            })?;
            if spec.close.is_some() {
                return Ok(out);
            }
            out
        }
    };
    let task = store.read(Some(&sess.actor), |c| compact(c, &id))?;
    Ok(json!({"ok": true, "result": result, "task": task}))
}

/// Required-at-creation fields of `ty` that the new issue shares with
/// `parent` and that were not given are copied from the parent.
fn inherit(c: &Ctx, parent: &str, ty: &str, assigns: &mut Vec<Assign>) -> Result<()> {
    let p = c.issue(parent)?;
    let t = c.schema.ty(ty)?.clone();
    for tf in &t.fields {
        if tf.required_at_create
            && !assigns.iter().any(|a| a.field == tf.field)
            && let Some(v) = p.fields.get(&tf.field)
        {
            assigns.push(Assign::set(tf.field.clone(), v.clone()));
        }
    }
    Ok(())
}

/// Field arguments of a composed tool that belong to its move or close step.
fn rest_assigns(
    c: &Ctx,
    a: &Map<String, Value>,
    spec: &crate::tools::ComposedAction,
) -> Vec<Assign> {
    a.iter()
        .filter(|(k, _)| {
            k.as_str() != "note" && c.schema.field(k).is_some() && !spec.set.contains(k)
        })
        .filter(|(k, _)| spec.create.as_ref().is_none_or(|cr| !cr.2.contains(k)))
        .map(|(k, v)| Assign::set(k.clone(), v.clone()))
        .collect()
}

/// The JSON object an agent sees for an error: code, message, hint and detail.
pub fn error_json(e: &Error) -> Value {
    let mut v = e.to_json();
    if e.code() == Code::NotPermitted
        && let Value::Object(m) = &mut v
    {
        m.entry("hint").or_insert(json!(
            "your claim on this issue is gone (released, expired or stolen); stop working on it"
        ));
    }
    v
}
