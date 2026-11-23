//! Rendering instructions and briefings with MiniJinja. Templates live in the
//! config dir and are named by their path relative to it, so templates can
//! `{% include "prompts/_rules.md.j2" %}`.

use serde_json::{Value, json};
use trk_core::config::ConfigDir;
use trk_core::error::{Code, Error, Result};

use crate::agent::AgentDef;
use crate::tools::ToolDef;

pub struct PromptInput<'a> {
    /// The issue object (`show --json` shape, with deps and derived).
    pub issue: &'a Value,
    pub agent: &'a AgentDef,
    pub tools: &'a [ToolDef],
    pub worktree: Option<&'a str>,
    pub base: &'a str,
    pub actor: &'a str,
    pub harness: Option<&'a str>,
}

impl PromptInput<'_> {
    fn context(&self, instructions: Option<&str>) -> Value {
        json!({
            "issue": self.issue,
            "agent": {"name": self.agent.name, "description": self.agent.description},
            "tools": self.tools.iter().map(|t| json!({"name": t.name, "description": t.description})).collect::<Vec<_>>(),
            "worktree": self.worktree,
            "base": self.base,
            "actor": self.actor,
            "harness": self.harness,
            "fold_instructions": instructions.is_some(),
            "instructions": instructions,
        })
    }
}

pub fn render_template(cfg: &ConfigDir, path: &str, ctx: &Value) -> Result<String> {
    let mut env = minijinja::Environment::new();
    env.set_loader(minijinja::path_loader(&cfg.path));
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

const DEFAULT_INSTRUCTIONS: &str = "You are working on one tracked issue. Use the trk tools to read it (get_task) and record progress (add_note). End the session with exactly one of: close (nothing more has to happen), wait (it is done once something outside this session happens), or hand_back (you cannot finish).";

/// Rules for the whole session: the MCP server's `instructions`.
pub fn instructions(cfg: &ConfigDir, input: &PromptInput) -> Result<String> {
    match &input.agent.instructions {
        Some(p) => render_template(cfg, p, &input.context(None)),
        None => Ok(DEFAULT_INSTRUCTIONS.to_string()),
    }
}

/// The task: the initial prompt. With `fold`, the instructions are appended
/// (for harnesses that ignore MCP server instructions).
pub fn briefing(cfg: &ConfigDir, input: &PromptInput, fold: Option<&str>) -> Result<String> {
    match &input.agent.briefing {
        Some(p) => render_template(cfg, p, &input.context(fold)),
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
