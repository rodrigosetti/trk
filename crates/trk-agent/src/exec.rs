//! Executing trk tools against the store. Every call is one transaction.

use serde_json::{Map, Value, json};
use trk_core::config::parse_duration;
use trk_core::error::{Code, Error, Result, bad_args};
use trk_core::ops::{NewIssue, WaitOpts};
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
    opt_arg(a, k).ok_or_else(|| bad_args(format!("missing required argument '{k}'")))
}

fn opt_arg<'a>(a: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    a.get(k)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

/// A compact view of the issue returned after each tool call.
pub fn compact(c: &Ctx, issue: &str) -> Result<Value> {
    let full = c.show(issue, With::default())?;
    Ok(json!({
        "id": full["id"],
        "state": full["state"],
        "claimed": !full["claim"].is_null(),
        "blocked_by": full["derived"]["blocked_by"],
    }))
}

pub fn call(store: &mut Store, sess: &Session, tool: &ToolDef, args: &Value) -> Result<Value> {
    let a = args_obj(args);
    let id = sess.issue.clone();
    let result: Value = match tool.action {
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
        Action::Link => {
            let url = str_arg(&a, "url")?.to_string();
            let label = opt_arg(&a, "label");
            let changed = store.write(&sess.actor, false, |c| c.link_add(&id, &url, label))?;
            json!({"linked": url, "changed": changed})
        }
        Action::ReportFollowup => {
            let title = str_arg(&a, "title")?.to_string();
            let description = str_arg(&a, "description")?.to_string();
            let priority = opt_arg(&a, "priority").map(String::from);
            let blocks_this = a
                .get("blocks_this")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let new_id = store.write(&sess.actor, false, |c| {
                let new_id = c.create(NewIssue {
                    title: title.clone(),
                    description,
                    priority,
                    issue_type: opt_arg(&a, "type").map(String::from),
                    epic: None,
                    blocks: if blocks_this {
                        vec![id.clone()]
                    } else {
                        vec![]
                    },
                    followup_of: Some(id.clone()),
                    ..Default::default()
                })?;
                c.note(
                    &id,
                    &format!("Filed follow-up {new_id}: {title}"),
                    None,
                    false,
                )?;
                Ok(new_id)
            })?;
            json!({"created": new_id, "blocks_this": blocks_this})
        }
        Action::HandBack => {
            let note = str_arg(&a, "note")?.to_string();
            store.write(&sess.actor, false, |c| c.unclaim(&id, false, Some(&note)))?;
            return Ok(
                json!({"handed_back": id, "message": "Your claim is released. Stop working on this issue."}),
            );
        }
        Action::Close => {
            let reason = str_arg(&a, "reason")?.to_string();
            let note = str_arg(&a, "note")?.to_string();
            store.write(&sess.actor, false, |c| {
                c.close(&id, &reason, Some(&note), false)
            })?;
            return Ok(json!({
                "closed": id, "reason": reason,
                "message": "The issue is closed and your claim released. Your work on it is done.",
            }));
        }
        Action::Wait => {
            let reason = str_arg(&a, "reason")?.to_string();
            let check = opt_arg(&a, "check").map(String::from);
            let interval = opt_arg(&a, "interval").map(parse_duration).transpose()?;
            let has_check = check.is_some();
            store.write(&sess.actor, false, |c| {
                c.wait(
                    &id,
                    WaitOpts {
                        check,
                        interval,
                        reason: Some(reason.clone()),
                        force: false,
                    },
                )
            })?;
            return Ok(json!({
                "waiting": id, "reason": reason,
                "message": if has_check {
                    "The issue is waiting and your claim released. trk runs the check on its schedule; your work in this session is done.".to_owned()
                } else if store.opts.chat_resume {
                        format!(
                            "The issue is waiting for a human and your claim released. This work cycle is done. \
                             After an actual unmarked human chat reply, report that reply verbatim through the MCP `resume` tool to restore your claim and continue. \
                             trk-delivered text wrapped in [trk-delivery ...] markers is not an eligible human reply. \
                             The human can also close or reopen the issue, or explicitly resume this live session with the command below \
                             (replace <your decision> with the decision).\n\
                             trk resume {id} -m \"<your decision>\""
                        )
                    } else {
                        format!(
                            "The issue is waiting for a human and your claim released. \
                             The human can close or reopen the issue, or resume this live session with a decision. \
                             End your chat question by saying plainly that a chat reply alone will not continue the work, \
                             followed by the command below (replace <your decision> with the decision). \
                             If the human replies only in chat, repeat the command rather than only recording a note.\n\
                             trk resume {id} -m \"<your decision>\""
                        )
                    }
            }));
        }
        Action::Resume => {
            let message = a
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| bad_args("missing required argument 'message'"))?;
            let claim = store.write(&sess.actor, false, |c| c.resume_chat(&id, message))?;
            json!({
                "resumed": id,
                "claim": claim,
                "message": "Your claim and tools are restored. Continue working on this issue.",
            })
        }
    };
    let task = store.read(Some(&sess.actor), |c| compact(c, &id))?;
    Ok(json!({"ok": true, "result": result, "task": task}))
}

/// The JSON object an agent sees for an error: code, message, hint and detail.
pub fn error_json(e: &Error) -> Value {
    let mut v = e.to_json();
    if e.code() == Code::NotPermitted
        && let Value::Object(m) = &mut v
    {
        m.entry("hint").or_insert(json!(
            "this write requires your active claim, except notes, links and follow-ups from your waiting session"
        ));
    }
    v
}
