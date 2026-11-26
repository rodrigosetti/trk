use std::collections::BTreeMap;

use serde_json::{Value, json};
use trk_core::config::Level;
use trk_core::error::{Code, Error, Result};
use trk_core::view::With;

use crate::AgentCmd;
use crate::env::Env;
use crate::render::Out;

fn diag_text(d: &[trk_core::config::Diagnostic]) -> String {
    d.iter()
        .map(|x| {
            format!(
                "{:<7} {}{}",
                match x.level {
                    Level::Error => "error",
                    Level::Warning => "warning",
                },
                x.file
                    .as_ref()
                    .map(|f| format!("{f}: "))
                    .unwrap_or_default(),
                x.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn agent_cmd(env: &Env, cmd: AgentCmd) -> Result<Out> {
    let local_config = || trk_agent::routing::RoutingConfig::new(&env.config);
    match cmd {
        AgentCmd::List => {
            let cfg = local_config();
            let agents = cfg.agents()?;
            let types = cfg.types()?;
            let profiles = trk_agent::harness::profiles(&env.user)?;
            let mut items = vec![];
            let mut text = String::new();
            for resolved in agents.values() {
                let a = &resolved.agent;
                let source = resolved.config.resolve(format!("agents/{}.toml", a.name));
                let source_layer = resolved.config.source_layer(&source);
                let templates =
                    trk_agent::prompt::agent_template_sources(&resolved.config, a, &env.user)?;
                let agent_types: Vec<_> = types.values().filter(|r| r.agent == a.name).collect();
                let models: BTreeMap<_, _> = profiles
                    .iter()
                    .map(|(name, profile)| (name, profile.resolve_model(a, name, None)))
                    .collect();
                let model_text = models
                    .iter()
                    .map(|(name, model)| format!("{name}={}", model.unwrap_or("(harness default)")))
                    .collect::<Vec<_>>()
                    .join(", ");
                let tools: Vec<String> = trk_agent::agent_tools(a, &env.user)
                    .into_iter()
                    .map(|t| t.name)
                    .collect();
                text.push_str(&format!(
                    "{:<12} {}\n             harness: {}  builtin_tools: {}\n             model: {}\n             tools: {}\n             source: {} ({})\n",
                    a.name,
                    a.description.clone().unwrap_or_default(),
                    a.harness.prefer.as_deref().unwrap_or_default().join(" > "),
                    a.harness.builtin_tools,
                    model_text,
                    tools.join(", "),
                    source.display(),
                    source_layer,
                ));
                for route in &agent_types {
                    text.push_str(&format!(
                        "             type {}: {} ({})\n",
                        route.issue_type,
                        route.type_source.display(),
                        route.type_layer,
                    ));
                }
                for (name, template) in &templates {
                    text.push_str(&format!(
                        "             template {name}: {} ({})\n",
                        template.source.display(),
                        template.layer,
                    ));
                }
                items.push(json!({"name": a.name, "description": a.description,
                                  "tools": tools,
                                  "model": a.harness.model, "models": models, "args": a.harness.args,
                                  "source": source, "source_layer": source_layer,
                                  "prompt_directory": resolved.config.path, "templates": templates,
                                  "types": agent_types,
                                  "harness": a.harness.prefer, "builtin_tools": a.harness.builtin_tools}));
            }
            if text.is_empty() {
                text = "no agents: run `trk install`".into();
            }
            Ok(Out::Text(text, Value::Array(items)))
        }
        AgentCmd::Check => {
            let cfg = local_config();
            let d = cfg.check(&env.user);
            let errors = d.iter().filter(|x| x.level == Level::Error).count();
            if errors > 0 {
                return Err(Error::new(
                    Code::ConfigInvalid,
                    format!("{errors} problem(s) in the agent configuration"),
                )
                .with_detail(
                    "problems",
                    json!(
                        d.iter()
                            .map(|x| format!(
                                "{}{}",
                                x.file
                                    .as_ref()
                                    .map(|f| format!("{f}: "))
                                    .unwrap_or_default(),
                                x.message
                            ))
                            .collect::<Vec<_>>()
                    ),
                ));
            }
            let mut resources = json!({"agents": [], "types": [], "templates": {}});
            let mut resource_text = String::new();
            let mut templates = BTreeMap::new();
            for resolved in cfg.agents()?.values() {
                let source = resolved
                    .config
                    .resolve(format!("agents/{}.toml", resolved.agent.name));
                let layer = resolved.config.source_layer(&source);
                resources["agents"].as_array_mut().unwrap().push(json!({
                    "name": resolved.agent.name, "source": source, "source_layer": layer,
                }));
                resource_text.push_str(&format!(
                    "\nagent {}: {} ({layer})",
                    resolved.agent.name,
                    source.display(),
                ));
                templates.extend(trk_agent::prompt::agent_template_sources(
                    &resolved.config,
                    &resolved.agent,
                    &env.user,
                )?);
            }
            for route in cfg.types()?.values() {
                let resolved = cfg.load(route)?;
                let prompts = trk_agent::prompt::type_prompts(
                    &resolved.config,
                    &resolved.agent,
                    &env.user,
                    route,
                )?;
                templates.extend(prompts.sources);
                resources["types"].as_array_mut().unwrap().push(json!({
                    "name": route.issue_type, "source": route.type_source,
                    "source_layer": route.type_layer,
                    "agent": route.agent, "guidance": prompts.guidance,
                }));
                resource_text.push_str(&format!(
                    "\ntype {}: {} ({})",
                    route.issue_type,
                    route.type_source.display(),
                    route.type_layer,
                ));
                if let Some(guidance) = &prompts.guidance {
                    resource_text.push_str(&format!("\n  guidance:\n{guidance}"));
                }
            }
            for (name, template) in &templates {
                resource_text.push_str(&format!(
                    "\ntemplate {name}: {} ({})",
                    template.source.display(),
                    template.layer,
                ));
            }
            resources["templates"] = serde_json::to_value(templates)?;
            let data = json!({"ok": errors == 0, "diagnostics": d, "resources": resources});
            let mut text = if d.is_empty() {
                "agents, templates and harness profiles are fine".to_string()
            } else {
                diag_text(&d)
            };
            text.push_str(&resource_text);
            Ok(Out::Text(text, data))
        }
        AgentCmd::Preview { agent, issue } => {
            let store = env.store()?;
            let epics = store.read(env.actor.as_deref(), |c| c.epics(false))?;
            let issue_json = match &issue {
                Some(id) => store.read(env.actor.as_deref(), |c| {
                    c.show(
                        id,
                        With {
                            deps: true,
                            notes: true,
                            ..Default::default()
                        },
                    )
                })?,
                None => trk_agent::check::sample_issue(&env.user),
            };
            let cfg = trk_agent::routing::RoutingConfig::new(&env.config);
            let routing = cfg.resolve(
                issue_json["type"].as_str().unwrap_or_default(),
                Some(&agent),
            )?;
            let resolved = cfg.load(&routing)?;
            let a = resolved.agent;
            let tools = trk_agent::agent_tools(&a, &env.user);
            let input = trk_agent::prompt::PromptInput {
                issue: &issue_json,
                epics: &epics,
                project_root: &env.root,
                config: &env.user,
                issue_type: &routing.issue_type,
                type_description: &routing.description,
                type_guidance: routing.guidance.as_deref(),
                agent: &a,
                tools: &tools,
                worktree: issue_json["claim"]["worktree"].as_str(),
                actor: "agent:<harness>:<session>",
                harness: None,
                base: &env.base(),
                verify: &env.user.check.verify,
            };
            let prompts = trk_agent::prompt::render_prompts(&resolved.config, &input, false)?;
            let instructions = prompts.instructions;
            let briefing = prompts.briefing;
            let tool_json: Vec<Value> = tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
                .collect();
            let others: Vec<Value> = a
                .mcp
                .iter()
                .map(|m| json!({"name": m.name, "command": m.command, "args": m.args}))
                .collect();
            let mut text = format!(
                "== routing ==\ntype: {} · agent: {}\ntype source: {} ({})\nagent source: {} ({})\nprompts: {}\n\n== instructions (MCP server) ==\n{instructions}\n\n== briefing (initial prompt) ==\n{briefing}\n",
                routing.issue_type,
                routing.agent,
                routing.type_source.display(),
                routing.type_layer,
                routing.agent_source.display(),
                routing.agent_layer,
                routing.prompt_directory.display(),
            );
            for (name, template) in &prompts.sources {
                text.push_str(&format!(
                    "\ntemplate {name}: {} ({})\n",
                    template.source.display(),
                    template.layer,
                ));
            }
            text.push_str("\n== trk tools ==\n");
            for tl in &tools {
                let req: Vec<String> = tl.input_schema["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                text.push_str(&format!(
                    "  {}({})\n      {}\n",
                    tl.name,
                    req.join(", "),
                    tl.description
                ));
            }
            if !others.is_empty() {
                text.push_str("\n== other MCP servers ==\n");
                for m in &a.mcp {
                    text.push_str(&format!(
                        "  {}: {} {}\n",
                        m.name,
                        m.command,
                        m.args.join(" ")
                    ));
                }
            }
            Ok(Out::Text(
                text,
                json!({"agent": a.name, "type": routing.issue_type, "routing": routing, "instructions": instructions, "briefing": briefing, "guidance": prompts.guidance, "templates": prompts.sources, "tools": tool_json, "mcp": others}),
            ))
        }
    }
}
