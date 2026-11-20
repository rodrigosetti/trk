//! The trk-agent half of `trk schema check` and `trk schema plan`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use trk_core::config::{ConfigDir, Diagnostic, UserConfig};
use trk_core::registry::{Schema, TypeDef};

use crate::agent::{AgentDef, BUILTIN_LEVELS, load_agent_file};
use crate::harness::{installed, profiles};
use crate::prompt::{PromptInput, briefing, instructions};
use crate::tools::{agent_tools, unmatched_includes};

/// A sample issue object for rendering templates during checks.
pub fn sample_issue(schema: &Schema, ty: &TypeDef) -> Value {
    let mut fields = Map::new();
    for f in &ty.fields {
        fields.insert(f.field.clone(), Value::Null);
    }
    let _ = schema;
    json!({
        "id": "proj-abc", "type": ty.name, "project": "proj", "title": "Sample", "context": "Sample context",
        "status": ty.first_active().map(|s| s.name.clone()).unwrap_or_default(), "status_category": "active",
        "resolution": null, "outcome": null, "owner": null, "links": [], "tags": [],
        "fields": fields, "claim": null, "deps": {"out": [], "in": []},
        "derived": {"ready": false, "blocked_by": [], "gates_unmet": [], "next": {}},
    })
}

/// Types routed to each agent.
pub fn routed_types<'a>(schema: &'a Schema, agent: &str) -> Vec<&'a TypeDef> {
    schema
        .types
        .values()
        .filter(|t| t.agent.as_deref() == Some(agent))
        .collect()
}

pub fn check_agents(cfg: &ConfigDir, schema: &Schema, user: &UserConfig) -> Vec<Diagnostic> {
    let mut d = vec![];
    let mut agents: BTreeMap<String, AgentDef> = BTreeMap::new();
    for p in cfg.files_in("agents", "toml") {
        let file = format!("agents/{}", p.file_name().unwrap().to_string_lossy());
        match load_agent_file(&p) {
            Ok(a) => {
                let stem = p.file_stem().unwrap().to_string_lossy().to_string();
                if stem != a.name {
                    d.push(Diagnostic::warning(
                        Some(&file),
                        format!("agent name '{}' differs from its file name", a.name),
                    ));
                }
                agents.insert(a.name.clone(), a);
            }
            Err(e) => d.push(Diagnostic::error(Some(&file), e.message())),
        }
    }
    for t in schema.types.values() {
        if let Some(a) = &t.agent
            && !agents.contains_key(a)
        {
            d.push(Diagnostic::error(
                Some(&format!("types/{}.toml", t.name)),
                format!("routing.agent '{a}' has no agents/{a}.toml"),
            ));
        }
    }
    let profs = match profiles(user) {
        Ok(p) => p,
        Err(e) => {
            d.push(Diagnostic::error(Some("config.toml"), e.message()));
            BTreeMap::new()
        }
    };
    for a in agents.values() {
        let file = format!("agents/{}.toml", a.name);
        let file = file.as_str();
        if !BUILTIN_LEVELS.contains(&a.harness.builtin_tools.as_str()) {
            d.push(Diagnostic::error(
                Some(file),
                format!(
                    "builtin_tools '{}' must be one of {}",
                    a.harness.builtin_tools,
                    BUILTIN_LEVELS.join(", ")
                ),
            ));
        }
        for h in &a.harness.prefer {
            match profs.get(h) {
                None => d.push(Diagnostic::warning(
                    Some(file),
                    format!("preferred harness '{h}' has no [harness.{h}] profile"),
                )),
                Some(p) => {
                    let lvl = a.harness.builtin_tools.as_str();
                    if lvl != "default" && !p.builtin_tools.contains_key(lvl) {
                        d.push(Diagnostic::warning(
                            Some(file),
                            format!("harness '{h}' cannot enforce builtin_tools = \"{lvl}\" (no builtin_tools.{lvl} in its profile)"),
                        ));
                    }
                    if !installed(&p.command) {
                        d.push(Diagnostic::warning(
                            Some(file),
                            format!("harness '{h}' command '{}' is not on PATH", p.command),
                        ));
                    }
                }
            }
        }
        for name in [&a.instructions, &a.briefing].into_iter().flatten() {
            if !cfg.path.join(name).exists() {
                d.push(Diagnostic::error(
                    Some(file),
                    format!("template '{name}' does not exist"),
                ));
            }
        }
        for (cname, c) in &a.trk_tools.composed {
            if let Some(cs) = &c.create {
                match schema.types.get(&cs.ty) {
                    None => d.push(Diagnostic::error(
                        Some(file),
                        format!("composed tool '{cname}' creates unknown type '{}'", cs.ty),
                    )),
                    Some(nt) => {
                        if !schema.dep_kinds.contains_key(&cs.edge) {
                            d.push(Diagnostic::error(
                                Some(file),
                                format!("composed tool '{cname}' uses unknown edge '{}'", cs.edge),
                            ));
                        }
                        for f in &cs.fields {
                            let ok = nt.type_field(f).is_some()
                                || schema.field(f).is_some_and(|x| x.shared);
                            if !ok {
                                d.push(Diagnostic::error(
                                    Some(file),
                                    format!(
                                        "composed tool '{cname}': type '{}' has no field '{f}'",
                                        cs.ty
                                    ),
                                ));
                            }
                        }
                        let routed = routed_types(schema, &a.name);
                        for tf in &nt.fields {
                            // Shared required fields are inherited from the current issue.
                            let inherited = !routed.is_empty()
                                && routed.iter().all(|t| t.type_field(&tf.field).is_some());
                            if tf.required_at_create && !cs.fields.contains(&tf.field) && !inherited
                            {
                                d.push(Diagnostic::error(
                                    Some(file),
                                    format!("composed tool '{cname}': '{}' needs field '{}' at creation; add it to `fields`", cs.ty, tf.field),
                                ));
                            }
                        }
                    }
                }
            }
        }
        let routed = routed_types(schema, &a.name);
        for t in &routed {
            for miss in unmatched_includes(schema, t, a) {
                d.push(Diagnostic::error(
                    Some(file),
                    format!(
                        "tool '{miss}' does not exist for type '{}', which routes to this agent",
                        t.name
                    ),
                ));
            }
            if let Some(cs) = a
                .trk_tools
                .composed
                .values()
                .filter_map(|c| c.create.as_ref())
                .next()
                && let (Some(nt), Ok(k)) = (schema.types.get(&cs.ty), schema.dep_kind(&cs.edge))
            {
                let (s, dd) = if cs.new_is_src() {
                    (&nt.name, &t.name)
                } else {
                    (&t.name, &nt.name)
                };
                if !k.allows(s, dd) {
                    d.push(Diagnostic::error(
                        Some(file),
                        format!("a composed tool creates a '{}' edge from {s} to {dd}, which '{}' does not allow", cs.edge, cs.edge),
                    ));
                }
            }
            let issue = sample_issue(schema, t);
            let tools = agent_tools(schema, t, a);
            let input = PromptInput {
                issue: &issue,
                agent: a,
                tools: &tools,
                worktree: Some("/tmp/wt"),
                actor: "agent:x:1",
                harness: None,
            };
            match instructions(cfg, &input) {
                Ok(i) => {
                    if let Err(e) = briefing(cfg, &input, Some(&i)) {
                        d.push(Diagnostic::error(Some(file), e.message()));
                    }
                }
                Err(e) => d.push(Diagnostic::error(Some(file), e.message())),
            }
        }
    }
    d
}

/// Per agent and routed type: tools added and removed going from `old` to `new`.
pub fn tool_diff(old: &Schema, new: &Schema, agents: &BTreeMap<String, AgentDef>) -> Vec<Value> {
    let mut out = vec![];
    for a in agents.values() {
        let mut types: BTreeSet<String> = routed_types(new, &a.name)
            .iter()
            .map(|t| t.name.clone())
            .collect();
        types.extend(routed_types(old, &a.name).iter().map(|t| t.name.clone()));
        for t in types {
            let names = |s: &Schema| -> BTreeSet<String> {
                s.types
                    .get(&t)
                    .map(|ty| agent_tools(s, ty, a).into_iter().map(|x| x.name).collect())
                    .unwrap_or_default()
            };
            let schemas = |s: &Schema| -> BTreeMap<String, Value> {
                s.types
                    .get(&t)
                    .map(|ty| {
                        agent_tools(s, ty, a)
                            .into_iter()
                            .map(|x| (x.name, x.input_schema))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let (o, n) = (names(old), names(new));
            let added: Vec<_> = n.difference(&o).cloned().collect();
            let removed: Vec<_> = o.difference(&n).cloned().collect();
            let (os, ns) = (schemas(old), schemas(new));
            let changed: Vec<_> = n
                .intersection(&o)
                .filter(|x| os.get(*x) != ns.get(*x))
                .cloned()
                .collect();
            if !added.is_empty() || !removed.is_empty() || !changed.is_empty() {
                out.push(json!({"agent": a.name, "type": t, "added": added, "removed": removed, "changed": changed}));
            }
        }
    }
    out
}
