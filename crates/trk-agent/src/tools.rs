//! The trk tools of a session. The set is fixed: one issue type, one state
//! machine. An agent may narrow it with `trk_tools.include`.

use serde::Serialize;
use serde_json::{Map, Value, json};
use trk_core::model::{CHECK_RETRY_EXIT, CLOSE_REASONS, PRIORITIES};

use crate::agent::AgentDef;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    GetTask,
    AddNote,
    ReportFollowup,
    HandBack,
    Close,
    Wait,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub action: Action,
}

fn object_schema(props: Vec<(&str, Value, bool)>) -> Value {
    let mut p = Map::new();
    let mut req = vec![];
    for (name, schema, required) in props {
        if required {
            req.push(Value::String(name.to_string()));
        }
        p.insert(name.to_string(), schema);
    }
    json!({"type": "object", "properties": p, "required": req, "additionalProperties": false})
}

fn text(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

/// Every trk tool, in the order sessions see them.
pub fn catalogue() -> Vec<ToolDef> {
    let tool = |name: &str, action: Action, description: String, input_schema: Value| ToolDef {
        name: name.to_string(),
        description,
        input_schema,
        action,
    };
    vec![
        tool(
            "get_task",
            Action::GetTask,
            "Read the issue: title, description, state, notes, and what blocks or is blocked by it.".into(),
            object_schema(vec![]),
        ),
        tool(
            "add_note",
            Action::AddNote,
            "Record progress, a finding or a decision on the issue. Notes are the issue's memory: the next session reads them.".into(),
            object_schema(vec![("body", text("The note, in markdown"), true)]),
        ),
        tool(
            "report_followup",
            Action::ReportFollowup,
            "File a new issue for work you found but will not do in this session.".into(),
            object_schema(vec![
                ("title", text("One line"), true),
                (
                    "description",
                    text("What is needed and why, with enough context to start cold"),
                    true,
                ),
                (
                    "priority",
                    json!({"type": "string", "enum": PRIORITIES, "description": "Default P2"}),
                    false,
                ),
                (
                    "blocks_this",
                    json!({"type": "boolean", "description": "true if this issue cannot finish until the new one is closed. Hand back after filing it."}),
                    false,
                ),
            ]),
        ),
        tool(
            "hand_back",
            Action::HandBack,
            "Give up the issue without finishing: it returns to open for another session. Say what is left and why.".into(),
            object_schema(vec![("note", text("What is done, what is left, and why you stopped"), true)]),
        ),
        tool(
            "close",
            Action::Close,
            "Finish the issue: nothing more has to happen. Use `wait` instead if it is only done once something else happens (a merge, a review, a decision).".into(),
            object_schema(vec![
                (
                    "reason",
                    json!({"type": "string", "enum": CLOSE_REASONS, "description": "done: the work is complete. abandoned: it will not be done."}),
                    true,
                ),
                ("note", text("The outcome: what was done, or why it was abandoned"), true),
            ]),
        ),
        tool(
            "wait",
            Action::Wait,
            format!(
                "End your session with the issue not yet done: it waits for something outside this session. \
                 With `check`, a shell command that trk runs later, on a schedule, in your worktree: \
                 exit 0 closes the issue as done; exit {CHECK_RETRY_EXIT} means not yet, check again after `interval`; \
                 any other exit returns the issue to open for a new session, with the command's output as a note. \
                 The command may do the remaining work itself (merge the branch) or only look (is the PR merged?). \
                 It runs unattended and may run more than once: make it non-interactive and safe to repeat. \
                 Without `check`, the issue waits for a human to close or reopen it."
            ),
            object_schema(vec![
                ("reason", text("What the issue is waiting for, for a human reading the queue"), true),
                (
                    "check",
                    text("Shell command, run with `sh -c` in your worktree. Environment: TRK_ISSUE, TRK_WORKTREE, TRK_REPO, TRK_BRANCH."),
                    false,
                ),
                (
                    "interval",
                    text("Time between checks, e.g. 2m, 1h, 1d. Default 5m."),
                    false,
                ),
            ]),
        ),
    ]
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

/// Does one `include` entry select `tool`? `@all` or a name, with `*` globs.
pub fn selects(entry: &str, tool: &ToolDef) -> bool {
    entry == "@all" || glob_match(entry, &tool.name)
}

/// The tools an agent gets: all of them, or those its `include` selects.
pub fn agent_tools(agent: &AgentDef) -> Vec<ToolDef> {
    let inc = &agent.trk_tools.include;
    catalogue()
        .into_iter()
        .filter(|t| inc.is_empty() || inc.iter().any(|e| selects(e, t)))
        .collect()
}

/// Include entries of `agent` that select nothing.
pub fn unmatched_includes(agent: &AgentDef) -> Vec<String> {
    let cat = catalogue();
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
        assert!(glob_match("report_*", "report_followup"));
        assert!(!glob_match("report_*", "close"));
        assert!(glob_match("*_task", "get_task"));
        assert!(glob_match("exact", "exact"));
    }

    #[test]
    fn six_tools() {
        let names: Vec<String> = catalogue().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "get_task",
                "add_note",
                "report_followup",
                "hand_back",
                "close",
                "wait"
            ]
        );
    }
}
