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
fn every_agent_gets_the_six_tools_unless_it_narrows_them() {
    let a = load_agent(&seed(), "implementer").unwrap();
    let tools = agent_tools(&a);
    assert_eq!(
        names(&tools),
        [
            "get_task",
            "add_note",
            "report_followup",
            "hand_back",
            "close",
            "wait"
        ]
    );
    let wait = tools.iter().find(|t| t.name == "wait").unwrap();
    assert_eq!(wait.input_schema["required"], serde_json::json!(["reason"]));
    assert!(wait.description.contains("exit 75"), "{}", wait.description);
    let close = tools.iter().find(|t| t.name == "close").unwrap();
    assert_eq!(
        close.input_schema["properties"]["reason"]["enum"],
        serde_json::json!(["done", "abandoned"])
    );
    let mut narrow = a.clone();
    narrow.trk_tools.include = vec!["get_*".into(), "close".into(), "nope".into()];
    assert_eq!(names(&agent_tools(&narrow)), ["get_task", "close"]);
    assert_eq!(trk_agent::tools::unmatched_includes(&narrow), ["nope"]);
}

#[test]
fn a_broken_agent_file_does_not_stop_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("good.toml"), "name = \"good\"\n").unwrap();
    std::fs::write(
        agents.join("old.toml"),
        "name = \"old\"\n[trk_tools.composed.x]\ndescription = \"gone\"\n",
    )
    .unwrap();
    let cfg = ConfigDir::new(dir.path());
    assert_eq!(load_agent(&cfg, "good").unwrap().name, "good");
    assert!(load_agent(&cfg, "old").is_err());
    let d = trk_agent::check::check_agents(&cfg, &UserConfig::default());
    assert!(
        d.iter()
            .any(|x| x.file.as_deref() == Some("agents/old.toml"))
    );
}

#[test]
fn seed_agents_check_clean() {
    let d = trk_agent::check::check_agents(&seed(), &user_config());
    let errors: Vec<_> = d.iter().filter(|x| x.level == Level::Error).collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn the_briefing_carries_description_notes_and_blockers() {
    let a = load_agent(&seed(), "implementer").unwrap();
    let tools = agent_tools(&a);
    let issue = serde_json::json!({
        "id": "orch-abc", "project": "orch", "title": "Add leases", "priority": "P1",
        "description": "Claims need leases.", "state": "claimed",
        "notes": [{"at": "2026-11-20T10:00:00Z", "actor": "human:a@b.c", "body": "The waiting check exited with 1"}],
        "deps": {"blocked_by": [], "blocks": [{"id": "orch-m1", "state": "open", "title": "MVP"}]},
    });
    let input = trk_agent::prompt::PromptInput {
        issue: &issue,
        agent: &a,
        tools: &tools,
        worktree: Some("/wt/orch-abc"),
        actor: "agent:omp:u1",
        harness: Some("omp"),
    };
    let rules = trk_agent::prompt::instructions(&seed(), &input).unwrap();
    assert!(rules.contains("Exit 75: not yet"), "{rules}");
    assert!(rules.contains("/wt/orch-abc"));
    let b = trk_agent::prompt::briefing(&seed(), &input, None).unwrap();
    assert!(b.contains("Claims need leases."), "{b}");
    assert!(b.contains("The waiting check exited with 1"), "{b}");
    assert!(b.contains("blocks orch-m1 (open): MVP"), "{b}");
    assert!(b.contains("priority P1"), "{b}");
    assert!(!b.contains("## Instructions"));
    let folded = trk_agent::prompt::briefing(&seed(), &input, Some(&rules)).unwrap();
    assert!(folded.contains("## Instructions"));
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
    assert_eq!(
        spec.argv[i + 1],
        "read,grep,glob,lsp,todo,mcp__trk_get_task,mcp__trk_add_note"
    );
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
    assert!(
        spec.argv
            .windows(2)
            .any(|w| w[0] == "--config" && w[1] == "/state/u1/omp.yml")
    );
    assert!(!spec.send_briefing);
    assert!(profiles["opencode"].sends_briefing());
}
