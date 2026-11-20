use std::collections::BTreeMap;

use trk_agent::harness::{self, LaunchInput};
use trk_agent::{agent_tools, load_agent};
use trk_core::config::{ConfigDir, Level, UserConfig};

fn seed() -> ConfigDir {
    ConfigDir::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../seed"))
}

fn user_config() -> UserConfig {
    let text = std::fs::read_to_string(seed().path.join("config.toml")).unwrap();
    toml::from_str(&text.replace("{{EMAIL_LINE}}", "email = \"a@b.c\"")).unwrap()
}

fn names(tools: &[trk_agent::ToolDef]) -> Vec<&str> {
    tools.iter().map(|t| t.name.as_str()).collect()
}

#[test]
fn implementer_tools_for_code_change() {
    let schema = seed().compile().unwrap();
    let a = load_agent(&seed(), "implementer").unwrap();
    let tools = agent_tools(&schema, &schema.types["code-change"], &a);
    assert_eq!(
        names(&tools),
        [
            "get_task",
            "add_note",
            "add_link",
            "report_followup",
            "hand_back",
            "resume_work",
            "submit_for_review",
            "close_as_abandoned",
            "set_branch",
            "set_pr_url",
            "request_review_followup"
        ]
    );
    let submit = tools
        .iter()
        .find(|t| t.name == "submit_for_review")
        .unwrap();
    assert_eq!(
        submit.input_schema["required"],
        serde_json::json!(["pr_url"])
    );
    assert_eq!(submit.input_schema["properties"]["pr_url"]["format"], "uri");
    assert!(
        submit.description.starts_with("Call after the PR is open"),
        "override applied"
    );
    let followup = tools
        .iter()
        .find(|t| t.name == "request_review_followup")
        .unwrap();
    assert_eq!(
        followup.input_schema["required"],
        serde_json::json!(["title", "context"])
    );
}

#[test]
fn close_tools_carry_requirements() {
    let schema = seed().compile().unwrap();
    let cat = trk_agent::catalogue(&schema, &schema.types["decision"]);
    let decided = cat.iter().find(|t| t.name == "close_as_decided").unwrap();
    assert_eq!(
        decided.input_schema["required"],
        serde_json::json!(["record_url", "outcome", "chosen", "rationale", "deciders"])
    );
    assert!(
        decided.description.contains("closed as approved"),
        "gate is described: {}",
        decided.description
    );
    let superseded = cat
        .iter()
        .find(|t| t.name == "close_as_superseded")
        .unwrap();
    assert!(
        superseded.input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("supersedes"))
    );
    let research = trk_agent::catalogue(&schema, &schema.types["research"]);
    let conf = research
        .iter()
        .find(|t| t.name == "set_confidence")
        .unwrap();
    assert_eq!(
        conf.input_schema["properties"]["value"]["enum"],
        serde_json::json!(["low", "medium", "high"])
    );
}

#[test]
fn seed_agents_check_clean() {
    let schema = seed().compile().unwrap();
    let d = trk_agent::check::check_agents(&seed(), &schema, &user_config());
    let errors: Vec<_> = d.iter().filter(|x| x.level == Level::Error).collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn launch_spec_for_omp() {
    let user = user_config();
    let profiles = harness::profiles(&user).unwrap();
    let a = load_agent(&seed(), "reviewer").unwrap();
    let mut env = BTreeMap::new();
    env.insert("TRK_DB".to_string(), "/db/trk.db".to_string());
    let spec = harness::build(LaunchInput {
        issue: "orch-abc",
        agent: &a,
        harness: "omp",
        profile: &profiles["omp"],
        actor: "agent:omp:u1",
        on_behalf_of: Some("human:a@b.c"),
        terminal: "trk-orch-abc",
        cwd: "/wt/orch-abc",
        worktree: Some("/wt/orch-abc"),
        session_dir: "/state/u1",
        trk_bin: "/bin/trk",
        trk_env: env,
        briefing: "Do it's thing".into(),
        instructions: "rules".into(),
        tools: vec!["get_task".into(), "add_note".into()],
    })
    .unwrap();
    // read-only builtin tools are inserted before the briefing argument.
    assert_eq!(spec.argv[0], "omp");
    assert_eq!(spec.argv[spec.argv.len() - 1], "Do it's thing");
    let i = spec.argv.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(spec.argv[i + 1], "read,grep,glob,lsp,todo,mcp__trk_get_task,mcp__trk_add_note");
    let mcp = &spec.files["/wt/orch-abc/.omp/mcp.json"];
    let v: serde_json::Value = serde_json::from_str(mcp).unwrap();
    assert_eq!(
        v["mcpServers"]["trk"]["args"],
        serde_json::json!([
            "mcp",
            "--issue",
            "orch-abc",
            "--actor",
            "agent:omp:u1",
            "--agent",
            "reviewer"
        ])
    );
    assert_eq!(v["mcpServers"]["trk"]["env"]["TRK_DB"], "/db/trk.db");
    assert!(
        spec.shell_command
            .starts_with("cd /wt/orch-abc && exec env TRK_ACTOR=agent:omp:u1 omp")
    );
    assert!(spec.shell_command.ends_with("'Do it'\\''s thing'"));
    // trk run sessions do not see the global trk skill.
    assert!(spec.files["/state/u1/omp.yml"].contains("ignoredSkills: [trk]"));
    assert!(spec.argv.windows(2).any(|w| w[0] == "--config" && w[1] == "/state/u1/omp.yml"));
    assert!(!spec.send_briefing);
    assert!(profiles["opencode"].sends_briefing());
}
