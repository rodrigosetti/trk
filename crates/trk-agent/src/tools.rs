//! The fixed trk tools and state machine. Types select agents, not workflows.
//! An agent may narrow its tools with `trk_tools.include`.

use serde::Serialize;
use serde_json::{Map, Value, json};
use trk_core::config::UserConfig;
use trk_core::model::{CHECK_RETRY_EXIT, CLOSE_REASONS, LINK_LABEL_MAX, PRIORITIES};

use crate::agent::AgentDef;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    GetTask,
    AddNote,
    Link,
    ReportFollowup,
    HandBack,
    Close,
    Wait,
    Resume,
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
pub fn catalogue(config: &UserConfig) -> Vec<ToolDef> {
    let check = &config.check;
    let tool = |name: &str, action: Action, description: String, input_schema: Value| ToolDef {
        name: name.to_string(),
        description,
        input_schema,
        action,
    };
    let failure = if check.max_wakes == 0 {
        "Failures reopen the issue for a new session; [check] max_wakes = 0 disables failure waking.".into()
    } else {
        format!(
            "Failures restore your claim and tools and wake your live session, up to {} times per session ([check] max_wakes); \
             if your session is gone, wake delivery fails, or the cap is reached, the issue reopens for a new session.",
            check.max_wakes
        )
    };
    let chat_resume = if config.wait.chat_resume {
        "After an actual unmarked human chat reply, your own waiting session may call the MCP `resume` tool with that reply verbatim to restore its claim and continue. trk-delivered text wrapped in [trk-delivery ...] markers is not an eligible human reply. End your chat question with the exact CLI resume alternative returned by this tool (with the real issue id)."
    } else {
        "Chat resume is disabled by [wait] chat_resume = false; a human must close, reopen, or explicitly resume it with `trk resume`. End your chat question with the exact resume command returned by this tool (with the real issue id), and say plainly that a chat reply alone will not continue the work. If the human replies only in chat, repeat the command rather than only recording a note."
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
            "link",
            Action::Link,
            "Attach a web address to the issue so people can open what your work produced from the dashboard. Linking the same url again changes its label.".into(),
            object_schema(vec![
                ("url", text("Absolute http(s) address"), true),
                (
                    "label",
                    text(&format!(
                        "Short display text, at most {LINK_LABEL_MAX} characters; the address is shown without one"
                    )),
                    false,
                ),
            ]),
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
                    "type",
                    text(&format!(
                        "Work type for the new issue; default {}",
                        config.issues.default_type
                    )),
                    false,
                ),
                (
                    "epic",
                    text("Existing active epic name; omit or leave empty for no epic. Agents cannot create, rename, or archive epics."),
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
                "End this work cycle with the issue not yet done: it waits for something outside this session. \
                 With `check`, a shell command that trk runs later, on a schedule, in your worktree: \
                 exit 0 closes the issue as done by default (`on_pass: close`); exit {CHECK_RETRY_EXIT} means not yet, check again after `interval`; \
                 any other exit or timeout records the output as a note. {failure} \
                 With `check` and `on_pass: wake`, a pass restores the same live session's claim and tools for a final work cycle without consuming the failure wake budget. \
                 End that cycle by closing or by waiting with `on_pass: close`; a second pass wake is refused. \
                 If the original session is gone, delivery fails, or its restored lease expires, the issue opens with a note that the check passed and the reason is still pending. \
                 Dependents remain blocked until you close the issue. \
                 It runs unattended under sh -c and may run more than once: make it non-interactive, non-blocking (no --watch) and safe to repeat. \
                 Without `check`, the issue waits for a human to close, reopen, or run `trk resume <ID> -m \"<your decision>\"` to continue the live session. {chat_resume} \
                 Chat resume never applies to checked waits; those require the check or a human using `trk resume`."
            ),
            object_schema(vec![
                ("reason", text("What the issue is waiting for, for a human reading the queue"), true),
                (
                    "check",
                    text("Shell command, run with `sh -c` in your worktree. Environment: TRK_ISSUE, TRK_WORKTREE, TRK_REPO, TRK_BRANCH, TRK_BASE (landing branch), TRK_VERIFY ([check] verify, default empty)."),
                    false,
                ),
                (
                    "interval",
                    text(&format!(
                        "Time between checks, e.g. 2m, 1h, 1d. Default {}.",
                        check.interval
                    )),
                    false,
                ),
                (
                    "on_pass",
                    json!({"type": "string", "enum": ["close", "wake"], "default": "close", "description": "On check success: close (default), or wake the same live session for a final work cycle. wake requires check and cannot be repeated after a pass wake."}),
                    false,
                ),
            ]),
        ),
        tool(
            "resume",
            Action::Resume,
            "Restore your own waiting session's claim after an actual unmarked human reply in this chat. \
             Only no-check waits with [wait] chat_resume enabled qualify. Pass the human's reply verbatim; \
             trk-delivered text wrapped in [trk-delivery ...] markers is never an eligible human chat reply. \
             This reports your observation of a human reply, not human authorization. \
             For checked waits or disabled chat resume, a human must use `trk resume`.".into(),
            object_schema(vec![("message", text("The actual unmarked human chat reply, verbatim and nonempty"), true)]),
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
pub fn agent_tools(agent: &AgentDef, config: &UserConfig) -> Vec<ToolDef> {
    let inc = &agent.trk_tools.include;
    catalogue(config)
        .into_iter()
        .filter(|t| inc.is_empty() || inc.iter().any(|e| selects(e, t)))
        .collect()
}

/// Include entries of `agent` that select nothing.
pub fn unmatched_includes(agent: &AgentDef, config: &UserConfig) -> Vec<String> {
    let cat = catalogue(config);
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
    fn eight_tools() {
        let names: Vec<String> = catalogue(&UserConfig::default())
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(
            names,
            [
                "get_task",
                "add_note",
                "link",
                "report_followup",
                "hand_back",
                "close",
                "wait",
                "resume"
            ]
        );
    }

    #[test]
    fn wait_describes_configured_check_behavior_without_workflow_policy() {
        for max_wakes in [0, 3] {
            let mut config = UserConfig::default();
            config.check.interval = "1h".into();
            config.check.max_wakes = max_wakes;
            let wait = catalogue(&config)
                .into_iter()
                .find(|t| t.name == "wait")
                .unwrap();
            assert_eq!(
                wait.input_schema["properties"]["interval"]["description"],
                "Time between checks, e.g. 2m, 1h, 1d. Default 1h."
            );
            assert_eq!(
                wait.input_schema["properties"]["on_pass"]["enum"],
                json!(["close", "wake"])
            );
            assert_eq!(
                wait.input_schema["properties"]["on_pass"]["default"],
                "close"
            );
            for lifecycle in [
                "on_pass: wake",
                "same live session's claim and tools",
                "without consuming the failure wake budget",
                "second pass wake is refused",
                "restored lease expires",
                "reason is still pending",
                "Dependents remain blocked",
            ] {
                assert!(wait.description.contains(lifecycle), "{lifecycle}");
            }
            let description = &wait.description;
            assert!(!description.contains("PR"), "{description}");
            assert!(
                !description.to_lowercase().contains("pull request"),
                "{description}"
            );
            if max_wakes == 0 {
                assert!(
                    description.contains("Failures reopen the issue"),
                    "{description}"
                );
                assert!(
                    description.contains("max_wakes = 0 disables failure waking"),
                    "{description}"
                );
                assert!(
                    !description.contains("wake your live session"),
                    "{description}"
                );
            } else {
                assert!(
                    description.contains("up to 3 times per session"),
                    "{description}"
                );
            }
        }
        let mut config = UserConfig::default();
        config.wait.chat_resume = false;
        let wait = catalogue(&config)
            .into_iter()
            .find(|t| t.name == "wait")
            .unwrap();
        assert!(wait.description.contains("chat_resume = false"));
        assert!(wait.description.contains("never applies to checked waits"));
    }

    #[test]
    fn resume_requires_a_verbatim_human_chat_reply() {
        let resume = catalogue(&UserConfig::default())
            .into_iter()
            .find(|t| t.name == "resume")
            .unwrap();
        assert_eq!(resume.action, Action::Resume);
        assert_eq!(resume.input_schema["required"], json!(["message"]));
        assert_eq!(
            resume.input_schema["properties"]["message"]["type"],
            "string"
        );
        assert_eq!(resume.input_schema["additionalProperties"], false);
        for restriction in [
            "own waiting session",
            "no-check waits",
            "[wait] chat_resume",
            "verbatim",
            "[trk-delivery",
            "not human authorization",
            "`trk resume`",
        ] {
            assert!(resume.description.contains(restriction), "{restriction}");
        }
    }
}
