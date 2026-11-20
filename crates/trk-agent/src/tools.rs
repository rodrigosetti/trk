//! Tool generation. A type offers a catalogue of trk tools generated from its
//! state machine and requirements; an agent selects from it.

use serde::Serialize;
use serde_json::{Map, Value, json};
use trk_core::registry::{Category, FieldDef, FieldKind, ReqKind, Schema, TypeDef};

use crate::agent::{AgentDef, Composed};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    GetTask,
    AddNote,
    AddLink,
    ReportFollowup,
    HandBack,
    Move {
        status: String,
        fields: Vec<String>,
    },
    Close {
        resolution: String,
        fields: Vec<String>,
        edges: Vec<String>,
    },
    SetField {
        field: String,
    },
    Composed {
        spec: Box<ComposedAction>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComposedAction {
    pub set: Vec<String>,
    pub create: Option<(String, String, Vec<String>, bool)>,
    pub move_to: Option<String>,
    pub close: Option<String>,
    pub note: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(flatten)]
    pub action: Action,
    /// "base" | "transition" | "close" | "set" | "composed"
    pub group: &'static str,
}

/// JSON Schema for one field value.
pub fn field_schema(def: &FieldDef) -> Value {
    let mut s = match def.kind {
        FieldKind::Text | FieldKind::Markdown | FieldKind::Repo => json!({"type": "string"}),
        FieldKind::Int => {
            let mut s = json!({"type": "integer"});
            if let Some(m) = def.config.get("min") {
                s["minimum"] = m.clone();
            }
            if let Some(m) = def.config.get("max") {
                s["maximum"] = m.clone();
            }
            s
        }
        FieldKind::Bool => json!({"type": "boolean"}),
        FieldKind::Date => json!({"type": "string", "format": "date", "description": "YYYY-MM-DD"}),
        FieldKind::Datetime => {
            json!({"type": "string", "description": "RFC 3339 timestamp, or \"now\""})
        }
        FieldKind::Url => json!({"type": "string", "format": "uri"}),
        FieldKind::Enum => json!({"type": "string", "enum": def.enum_values()}),
        FieldKind::Actor => {
            json!({"type": "string", "description": "human:<email>, agent:<harness>:<id>, or \"me\""})
        }
        FieldKind::Link => json!({
            "type": "object",
            "properties": {"kind": {"type": "string"}, "url": {"type": "string", "format": "uri"}, "note": {"type": "string"}},
            "required": ["url"],
        }),
        FieldKind::List => {
            let item = FieldDef {
                name: def.name.clone(),
                kind: def.list_item(),
                config: def.config.clone(),
                shared: false,
                description: None,
            };
            json!({"type": "array", "items": field_schema(&item)})
        }
    };
    if let Some(d) = &def.description
        && s.get("description").is_none()
    {
        s["description"] = json!(d);
    }
    s
}

fn object_schema(props: Vec<(String, Value, bool)>) -> Value {
    let mut p = Map::new();
    let mut req = vec![];
    for (name, schema, required) in props {
        if required {
            req.push(Value::String(name.clone()));
        }
        p.insert(name, schema);
    }
    json!({"type": "object", "properties": p, "required": req, "additionalProperties": false})
}

fn note_prop() -> (String, Value, bool) {
    (
        "note".into(),
        json!({"type": "string", "description": "Optional note recorded with this step"}),
        false,
    )
}

fn field_prop(schema: &Schema, f: &str, required: bool) -> (String, Value, bool) {
    let s = schema
        .field(f)
        .map(field_schema)
        .unwrap_or_else(|| json!({"type": "string"}));
    (f.to_string(), s, required)
}

/// Fields required to enter `to` (with `resolution` when closing), and
/// out-edge kinds required.
fn requirement_params(
    ty: &TypeDef,
    to: &str,
    resolution: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut fields = vec![];
    let mut edges = vec![];
    for r in ty.requirements_for(to, resolution) {
        match &r.kind {
            ReqKind::Field { field } | ReqKind::MinItems { field, .. } => {
                if !fields.contains(field) {
                    fields.push(field.clone())
                }
            }
            ReqKind::OutEdge { dep_kind } => edges.push(dep_kind.clone()),
            ReqKind::InEdgesClosed { .. } => {}
        }
    }
    (fields, edges)
}

fn gate_note(ty: &TypeDef, to: &str, resolution: Option<&str>) -> String {
    let gates: Vec<String> = ty
        .requirements_for(to, resolution)
        .filter_map(|r| match &r.kind {
            ReqKind::InEdgesClosed {
                dep_kind,
                src_resolution,
            } => Some(match src_resolution {
                Some(sr) => format!("every incoming {dep_kind} issue is closed as {sr}"),
                None => format!("every incoming {dep_kind} issue is closed"),
            }),
            _ => None,
        })
        .collect();
    if gates.is_empty() {
        String::new()
    } else {
        format!(" Allowed only when {}.", gates.join(" and "))
    }
}

/// Every trk tool a type offers.
pub fn catalogue(schema: &Schema, ty: &TypeDef) -> Vec<ToolDef> {
    let mut out = vec![
        ToolDef {
            name: "get_task".into(),
            description: format!(
                "Return the current state of this {}: fields, status, dependencies, notes, and derived.next (the fields still missing for each next step).",
                ty.name
            ),
            input_schema: object_schema(vec![]),
            action: Action::GetTask,
            group: "base",
        },
        ToolDef {
            name: "add_note".into(),
            description: "Append a note to this issue's history: progress, findings, decisions, commands run.".into(),
            input_schema: object_schema(vec![("body".into(), json!({"type": "string", "description": "Markdown text"}), true)]),
            action: Action::AddNote,
            group: "base",
        },
        ToolDef {
            name: "add_link".into(),
            description: "Attach a link as evidence (PR, document, thread, log).".into(),
            input_schema: object_schema(vec![
                ("url".into(), json!({"type": "string", "format": "uri"}), true),
                ("kind".into(), json!({"type": "string", "description": "e.g. pr, doc, slack, log"}), false),
                ("note".into(), json!({"type": "string"}), false),
            ]),
            action: Action::AddLink,
            group: "base",
        },
        ToolDef {
            name: "report_followup".into(),
            description: "File a new issue for work discovered while doing this one (it gets a discovered-from edge to this issue). Use it instead of doing out-of-scope work.".into(),
            input_schema: object_schema(vec![
                ("type".into(), json!({"type": "string", "enum": schema.types.keys().collect::<Vec<_>>()}), true),
                ("title".into(), json!({"type": "string"}), true),
                ("context".into(), json!({"type": "string", "description": "Background, motivation, constraints (markdown)"}), true),
                ("fields".into(), json!({"type": "object", "description": "Other fields of the new issue, e.g. {\"question\": \"…\"} for research. Required fields it shares with this issue (such as repo) are copied when omitted."}), false),
            ]),
            action: Action::ReportFollowup,
            group: "base",
        },
        ToolDef {
            name: "hand_back".into(),
            description: "Give the issue back unfinished: releases your claim and returns it to open. Explain what is done and what is left.".into(),
            input_schema: object_schema(vec![("note".into(), json!({"type": "string"}), true)]),
            action: Action::HandBack,
            group: "base",
        },
    ];

    // One tool per transition into an intermediate status.
    let mut targets: Vec<String> = vec![];
    for s in ty
        .statuses
        .iter()
        .filter(|s| s.category == Category::Active)
    {
        let reachable = ty.transitions.iter().any(|(from, tos)| {
            ty.category_of(from) == Some(Category::Active) && tos.contains(&s.name)
        });
        if reachable {
            targets.push(s.name.clone());
        }
    }
    for to in targets {
        let (fields, _) = requirement_params(ty, &to, None);
        let mut props: Vec<_> = fields.iter().map(|f| field_prop(schema, f, true)).collect();
        props.push(note_prop());
        let desc = ty
            .tools
            .get(&to)
            .and_then(|t| t.description.clone())
            .unwrap_or_else(|| format!("Move this {} to '{to}'.", ty.name));
        let req = if fields.is_empty() {
            String::new()
        } else {
            format!(" Requires: {}.", fields.join(", "))
        };
        out.push(ToolDef {
            name: ty.transition_tool_name(&to),
            description: format!("{desc}{req}{}", gate_note(ty, &to, None)),
            input_schema: object_schema(props),
            action: Action::Move {
                status: to.clone(),
                fields,
            },
            group: "transition",
        });
    }

    // One tool per close resolution.
    if let Some(closed) = ty.closed_status() {
        for r in &ty.resolutions {
            let (fields, edges) = requirement_params(ty, &closed.name, Some(r));
            let mut props: Vec<_> = fields.iter().map(|f| field_prop(schema, f, true)).collect();
            for e in &edges {
                props.push((
                    e.replace('-', "_"),
                    json!({"type": "string", "description": format!("Id of the issue this one {e}")}),
                    true,
                ));
            }
            props.push(note_prop());
            let req = if fields.is_empty() && edges.is_empty() {
                String::new()
            } else {
                let mut all = fields.clone();
                all.extend(edges.iter().map(|e| format!("a {e} edge")));
                format!(" Requires: {}.", all.join(", "))
            };
            out.push(ToolDef {
                name: format!("close_as_{r}"),
                description: format!(
                    "Close this {} with resolution '{r}'.{req}{}",
                    ty.name,
                    gate_note(ty, &closed.name, Some(r))
                ),
                input_schema: object_schema(props),
                action: Action::Close {
                    resolution: r.clone(),
                    fields,
                    edges,
                },
                group: "close",
            });
        }
    }

    // One setter per type field, plus the shared outcome.
    let mut settable: Vec<String> = ty
        .fields
        .iter()
        .map(|f| f.field.clone())
        .filter(|f| f != "agent")
        .collect();
    settable.push("outcome".into());
    for f in settable {
        let Some(def) = schema.field(&f) else {
            continue;
        };
        let desc = def
            .description
            .clone()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();
        out.push(ToolDef {
            name: format!("set_{f}"),
            description: format!(
                "Set the {} field `{f}`{desc}. An empty value clears it.",
                ty.name
            ),
            input_schema: object_schema(vec![("value".into(), field_schema(def), true)]),
            action: Action::SetField { field: f.clone() },
            group: "set",
        });
    }
    out
}

fn glob_match(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((pre, post)) => {
            s.len() >= pre.len() + post.len()
                && s.starts_with(pre)
                && glob_match_rest(post, &s[pre.len()..])
        }
    }
}

fn glob_match_rest(pat: &str, s: &str) -> bool {
    if !pat.contains('*') {
        return s.ends_with(pat);
    }
    let (pre, post) = pat.split_once('*').unwrap();
    (0..=s.len()).any(|i| s[i..].starts_with(pre) && glob_match_rest(post, &s[i + pre.len()..]))
}

/// Does one `include` entry select `tool`?
pub fn selects(entry: &str, tool: &ToolDef) -> bool {
    match entry {
        "@base" => tool.group == "base",
        "@transitions" => tool.group == "transition",
        "@close" => tool.group == "close",
        "@set" => tool.group == "set",
        "@all" => true,
        e => glob_match(e, &tool.name),
    }
}

fn composed_tool(schema: &Schema, ty: &TypeDef, name: &str, c: &Composed) -> ToolDef {
    let mut props = vec![];
    for f in &c.set {
        props.push(field_prop(schema, f, false));
    }
    let mut create = None;
    if let Some(cs) = &c.create {
        for f in &cs.fields {
            if !props.iter().any(|(n, _, _)| n == f) {
                let required = f == "title"
                    || f == "context"
                    || schema
                        .types
                        .get(&cs.ty)
                        .and_then(|t| t.type_field(f))
                        .is_some_and(|tf| tf.required_at_create);
                props.push(field_prop(schema, f, required));
            }
        }
        create = Some((
            cs.ty.clone(),
            cs.edge.clone(),
            cs.fields.clone(),
            cs.new_is_src(),
        ));
    }
    let mut extra_fields = vec![];
    if let Some(to) = &c.move_to {
        let (fields, _) = requirement_params(ty, to, None);
        extra_fields.extend(fields);
    }
    if let (Some(r), Some(closed)) = (&c.close, ty.closed_status()) {
        let (fields, _) = requirement_params(ty, &closed.name, Some(r));
        extra_fields.extend(fields);
    }
    for f in extra_fields {
        if !props.iter().any(|(n, _, _)| *n == f) {
            props.push(field_prop(schema, &f, true));
        }
    }
    if c.note {
        props.push(note_prop());
    }
    ToolDef {
        name: name.to_string(),
        description: c.description.clone(),
        input_schema: object_schema(props),
        action: Action::Composed {
            spec: Box::new(ComposedAction {
                set: c.set.clone(),
                create,
                move_to: c.move_to.clone(),
                close: c.close.clone(),
                note: c.note,
            }),
        },
        group: "composed",
    }
}

/// The tools an agent gets on an issue of `ty`, in catalogue order, then composed.
pub fn agent_tools(schema: &Schema, ty: &TypeDef, agent: &AgentDef) -> Vec<ToolDef> {
    let mut out: Vec<ToolDef> = catalogue(schema, ty)
        .into_iter()
        .filter(|t| agent.trk_tools.include.iter().any(|e| selects(e, t)))
        .collect();
    for t in &mut out {
        if let Some(o) = agent.trk_tools.overrides.get(&t.name)
            && let Some(d) = &o.description
        {
            t.description = d.clone();
        }
    }
    for (name, c) in &agent.trk_tools.composed {
        out.push(composed_tool(schema, ty, name, c));
    }
    out
}

/// Include entries of `agent` that select nothing from `ty`'s catalogue.
pub fn unmatched_includes(schema: &Schema, ty: &TypeDef, agent: &AgentDef) -> Vec<String> {
    let cat = catalogue(schema, ty);
    agent
        .trk_tools
        .include
        .iter()
        .filter(|e| !cat.iter().any(|t| selects(e, t)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("close_as_*", "close_as_merged"));
        assert!(!glob_match("close_as_*", "set_branch"));
        assert!(glob_match("*_url", "set_pr_url"));
        assert!(glob_match("set_*_url", "set_pr_url"));
        assert!(!glob_match("set_*_url", "set_branch"));
        assert!(glob_match("exact", "exact"));
    }
}
