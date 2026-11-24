//! Rendering instructions and briefings with MiniJinja. Templates live in the
//! config dir and are named by their path relative to it, so templates can
//! `{% include "prompts/_rules.md.j2" %}`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};
use trk_core::config::{ConfigDir, UserConfig};
use trk_core::error::{Code, Error, Result};

use crate::agent::AgentDef;
use crate::tools::ToolDef;

pub struct PromptInput<'a> {
    /// The issue object (`show --json` shape, with deps and derived).
    pub issue: &'a Value,
    pub project_root: &'a Path,
    pub config: &'a UserConfig,
    /// Metadata from the resolved type, independent of an agent override.
    pub issue_type: &'a str,
    pub type_description: &'a str,
    pub agent: &'a AgentDef,
    pub tools: &'a [ToolDef],
    pub worktree: Option<&'a str>,
    pub base: &'a str,
    pub verify: &'a str,
    pub actor: &'a str,
    pub harness: Option<&'a str>,
}

impl PromptInput<'_> {
    fn context(&self, instructions: Option<&str>) -> Value {
        json!({
            "issue": self.issue,
            "project": {
                "name": self.project_root.file_name().unwrap_or_default().to_string_lossy(),
                "root": self.project_root,
                "prefix": self.config.issues.prefix,
            },
            "type": {"name": self.issue_type, "description": self.type_description},
            "config": {
                "check": {
                    "interval": self.config.check.interval,
                    "timeout": self.config.check.timeout,
                    "max_wakes": self.config.check.max_wakes,
                },
                "claims": {"agent_lease": self.config.claims.agent_lease},
                "run": {
                    "worktrees": self.config.run.worktrees,
                    "landing": self.config.run.landing,
                },
            },
            "agent": {"name": self.agent.name, "description": self.agent.description},
            "tools": self.tools.iter().map(|t| json!({"name": t.name, "description": t.description})).collect::<Vec<_>>(),
            "worktree": self.worktree,
            "base": self.base,
            "verify": self.verify,
            "actor": self.actor,
            "harness": self.harness,
            "fold_instructions": instructions.is_some(),
            "instructions": instructions,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TemplateSource {
    pub source: PathBuf,
    pub layer: &'static str,
}

type Sources = Arc<Mutex<BTreeMap<String, TemplateSource>>>;

fn environment(cfg: &ConfigDir, sources: Option<Sources>) -> minijinja::Environment<'static> {
    let mut env = minijinja::Environment::new();
    let project = minijinja::path_loader(&cfg.path);
    let user = cfg.user_path.as_ref().map(minijinja::path_loader);
    let source_config = sources.as_ref().map(|_| cfg.clone());
    env.set_loader(move |name| {
        let (template, from_user) = match project(name)? {
            Some(template) => (Some(template), false),
            None => match &user {
                Some(load) => (load(name)?, true),
                None => (None, false),
            },
        };
        if template.is_some()
            && let (Some(sources), Some(cfg)) = (&sources, &source_config)
        {
            let (root, layer) = if from_user {
                (cfg.user_path.as_ref().unwrap(), "user")
            } else {
                (&cfg.path, "project")
            };
            sources.lock().unwrap().insert(
                name.to_string(),
                TemplateSource {
                    layer,
                    source: root.join(name),
                },
            );
        }
        Ok(template)
    });
    env
}

pub fn render_template(cfg: &ConfigDir, path: &str, ctx: &Value) -> Result<String> {
    render(&environment(cfg, None), path, ctx)
}

fn render(env: &minijinja::Environment<'_>, path: &str, ctx: &Value) -> Result<String> {
    let tmpl = env
        .get_template(path)
        .map_err(|e| Error::new(Code::ConfigInvalid, format!("template {path}: {e}")))?;
    tmpl.render(minijinja::Value::from(minijinja::value::Serde(ctx)))
        .map_err(|e| {
            let mut msg = format!("template {path}: {e}");
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                msg.push_str(&format!(": {s}"));
                src = s.source();
            }
            Error::new(Code::ConfigInvalid, msg)
        })
}

pub struct RenderedPrompts {
    pub instructions: String,
    pub briefing: String,
    pub sources: BTreeMap<String, TemplateSource>,
}

/// Render once while recording the templates actually used, including includes,
/// imports and inheritance. Folding must match the selected harness.
pub fn render_prompts(
    cfg: &ConfigDir,
    input: &PromptInput,
    fold_instructions: bool,
) -> Result<RenderedPrompts> {
    let sources = Sources::default();
    let env = environment(cfg, Some(sources.clone()));
    let instructions = render_instructions(&env, input)?;
    let fold = fold_instructions.then_some(instructions.as_str());
    let briefing = render_briefing(&env, input, fold)?;
    let sources = std::mem::take(&mut *sources.lock().unwrap());
    Ok(RenderedPrompts {
        instructions,
        briefing,
        sources,
    })
}

/// Inspect with instructions folded, as in the representative agent check context.
pub fn template_sources(
    cfg: &ConfigDir,
    input: &PromptInput,
) -> Result<BTreeMap<String, TemplateSource>> {
    Ok(render_prompts(cfg, input, true)?.sources)
}

/// Inspect an agent's templates using the same representative context as `agent check`.
pub fn agent_template_sources(
    cfg: &ConfigDir,
    agent: &AgentDef,
    user: &UserConfig,
) -> Result<BTreeMap<String, TemplateSource>> {
    let issue = crate::check::sample_issue();
    let tools = crate::tools::agent_tools(agent, &user.check);
    template_sources(
        cfg,
        &PromptInput {
            issue: &issue,
            project_root: cfg.path.parent().unwrap_or(&cfg.path),
            config: user,
            issue_type: "code-change",
            type_description: "Sample type description",
            agent,
            tools: &tools,
            worktree: None,
            base: user.run.base.as_deref().unwrap_or("main"),
            verify: &user.check.verify,
            actor: "agent:x:1",
            harness: None,
        },
    )
}

const DEFAULT_INSTRUCTIONS: &str = "You are working on one tracked issue. Use the trk tools to read it (get_task) and record progress (add_note). End the session with exactly one of: close (nothing more has to happen), wait (it is done once something outside this session happens), or hand_back (you cannot finish).";

/// Rules for the whole session: the MCP server's `instructions`.
pub fn instructions(cfg: &ConfigDir, input: &PromptInput) -> Result<String> {
    render_instructions(&environment(cfg, None), input)
}

fn render_instructions(env: &minijinja::Environment<'_>, input: &PromptInput) -> Result<String> {
    match &input.agent.instructions {
        Some(p) => render(env, p, &input.context(None)),
        None => Ok(DEFAULT_INSTRUCTIONS.to_string()),
    }
}

/// The task: the initial prompt. With `fold`, the instructions are appended
/// (for harnesses that ignore MCP server instructions).
pub fn briefing(cfg: &ConfigDir, input: &PromptInput, fold: Option<&str>) -> Result<String> {
    render_briefing(&environment(cfg, None), input, fold)
}

fn render_briefing(
    env: &minijinja::Environment<'_>,
    input: &PromptInput,
    fold: Option<&str>,
) -> Result<String> {
    match &input.agent.briefing {
        Some(p) => render(env, p, &input.context(fold)),
        None => {
            let issue = input.issue;
            let mut s = format!(
                "Work on {} \"{}\".\n\n{}\n",
                issue["id"].as_str().unwrap_or(""),
                issue["title"].as_str().unwrap_or(""),
                issue["description"].as_str().unwrap_or("")
            );
            if let Some(i) = fold {
                s.push_str("\n## Instructions\n\n");
                s.push_str(i);
            }
            Ok(s)
        }
    }
}
