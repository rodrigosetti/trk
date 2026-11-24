use std::collections::BTreeMap;

use trk_agent::harness::{self, LaunchInput};
use trk_agent::{agent_tools, load_agent};
use trk_core::config::{ConfigDir, Level, UserConfig};

fn seed() -> (tempfile::TempDir, ConfigDir) {
    seed_preset("default")
}

fn seed_preset(name: &str) -> (tempfile::TempDir, ConfigDir) {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in trk_core::seed::preset(name).unwrap() {
        let rendered = (path == "config.toml").then(|| {
            std::str::from_utf8(content)
                .unwrap()
                .replace("{{EMAIL_LINE}}", "email = \"a@b.c\"")
                .replace("{{PREFIX}}", "proj")
                .replace("{{BASE}}", "\"main\"")
        });
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            rendered.as_deref().map(str::as_bytes).unwrap_or(content),
        )
        .unwrap();
    }
    let config = ConfigDir::with_user_dir(dir.path(), None);
    (dir, config)
}

fn user_config() -> UserConfig {
    toml::from_str(
        &trk_core::seed::CONFIG_TOML
            .replace("{{EMAIL_LINE}}", "email = \"a@b.c\"")
            .replace("{{PREFIX}}", "proj")
            .replace(
                "{{DEFAULT_TYPE}}",
                &trk_core::config::IssuesSection::default().default_type,
            )
            .replace("{{BASE}}", "\"main\""),
    )
    .unwrap()
}

fn names(tools: &[trk_agent::ToolDef]) -> Vec<&str> {
    tools.iter().map(|t| t.name.as_str()).collect()
}

#[test]
fn every_agent_gets_the_seven_tools_unless_it_narrows_them() {
    let (_dir, seed) = seed();
    let a = load_agent(&seed, "implementer").unwrap();
    let tools = agent_tools(&a, &user_config());
    assert_eq!(
        names(&tools),
        [
            "get_task",
            "add_note",
            "link",
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
    assert_eq!(
        names(&agent_tools(&narrow, &user_config())),
        ["get_task", "close"]
    );
    assert_eq!(
        trk_agent::tools::unmatched_includes(&narrow, &user_config()),
        ["nope"]
    );
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
    let cfg = ConfigDir::with_user_dir(dir.path(), None);
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
    let (_dir, seed) = seed();
    let d = trk_agent::check::check_agents(&seed, &user_config());
    let errors: Vec<_> = d.iter().filter(|x| x.level == Level::Error).collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn sample_issue_uses_the_configured_default_type() {
    let mut config = user_config();
    config.issues.default_type = "task".into();
    assert_eq!(trk_agent::check::sample_issue(&config)["type"], "task");
}

#[test]
fn check_reports_a_missing_default_type_definition() {
    let (_dir, seed) = seed();
    let mut user = user_config();
    user.issues.default_type = "reserch".into();
    let diagnostics = trk_agent::check::check_agents(&seed, &user);
    assert!(diagnostics.iter().any(|d| {
        d.level == Level::Error
            && d.file.as_deref() == Some("config.toml")
            && d.message.contains("issues.default_type 'reserch'")
            && d.message.contains(".trk/types/reserch.toml")
    }));
    std::fs::write(
        seed.path.join("types/reserch.toml"),
        "description = \"Custom research\"\nagent = \"researcher\"\n",
    )
    .unwrap();
    assert!(
        trk_agent::check::check_agents(&seed, &user)
            .iter()
            .all(|d| d.level != Level::Error)
    );
}

#[test]
fn landing_selection_only_changes_implementer_instructions() {
    for preset in ["default", "minimal"] {
        let (_dir, seed) = seed_preset(preset);
        std::fs::write(
            seed.path.join("prompts/_landing.gitlab.md.j2"),
            "Open a GitLab merge request targeting {{ base }} ({{ config.run.landing }}).",
        )
        .unwrap();
        let issue = trk_agent::check::sample_issue(&user_config());
        for landing in ["merge", "pr", "gitlab", "legacy"] {
            let mut config = user_config();
            if landing == "legacy" {
                let old = toml::to_string(&config)
                    .unwrap()
                    .replace("landing = \"merge\"\n", "");
                config = toml::from_str(&old).unwrap();
            } else {
                config.run.landing = landing.into();
            }
            let diagnostics = trk_agent::check::check_agents(&seed, &config);
            assert!(
                diagnostics.iter().all(|d| d.level != Level::Error),
                "{diagnostics:#?}"
            );
            let agents: &[&str] = if preset == "default" {
                &["implementer", "researcher", "reviewer"]
            } else {
                &["implementer"]
            };
            for &name in agents {
                let agent = load_agent(&seed, name).unwrap();
                let tools = agent_tools(&agent, &config);
                let input = trk_agent::prompt::PromptInput {
                    issue: &issue,
                    project_root: std::path::Path::new("/project"),
                    config: &config,
                    issue_type: "code-change",
                    type_description: "code change",
                    agent: &agent,
                    tools: &tools,
                    worktree: Some("/wt/proj-abc"),
                    base: "develop",
                    verify: "",
                    actor: "agent:omp:u1",
                    harness: Some("omp"),
                };
                let rules = trk_agent::prompt::instructions(&seed, &input).unwrap();
                let implementer = name == "implementer";
                assert_eq!(
                    rules.matches("git merge --ff-only").count(),
                    usize::from(implementer && matches!(landing, "merge" | "legacy")),
                    "{name}/{landing}: {rules}"
                );
                assert_eq!(
                    rules.matches("gh pr view").count(),
                    usize::from(implementer && landing == "pr"),
                    "{name}/{landing}: {rules}"
                );
                assert_eq!(
                    rules
                        .matches("GitLab merge request targeting develop (gitlab)")
                        .count(),
                    usize::from(implementer && landing == "gitlab"),
                    "{name}/{landing}: {rules}"
                );
                if implementer && landing == "pr" {
                    assert!(
                        rules.contains("pull request targeting `develop`"),
                        "{rules}"
                    );
                    assert!(!rules.contains("Merge your branch"), "{rules}");
                }
            }
        }
    }
}

#[test]
fn missing_landing_template_is_reported_even_without_an_implementer() {
    let (_dir, seed) = seed();
    std::fs::remove_file(seed.path.join("agents/implementer.toml")).unwrap();
    let mut config = user_config();
    config.run.landing = "missing".into();
    let diagnostics = trk_agent::check::check_agents(&seed, &config);
    assert!(
        diagnostics.iter().any(|d| d.level == Level::Error
            && d.file.as_deref() == Some("config.toml")
            && d.message.contains("run.landing 'missing'")
            && d.message.contains("prompts/_landing.missing.md.j2")),
        "{diagnostics:#?}"
    );
}

#[test]
fn the_briefing_carries_description_notes_and_blockers() {
    let (_dir, seed) = seed();
    let a = load_agent(&seed, "implementer").unwrap();
    let tools = agent_tools(&a, &user_config());
    let issue = serde_json::json!({
        "id": "orch-abc", "project": "orch", "title": "Add leases", "priority": "P1",
        "description": "Claims need leases.", "state": "claimed",
        "notes": [{"at": "2026-11-20T10:00:00Z", "actor": "human:a@b.c", "body": "The waiting check exited with 1"}],
        "deps": {"blocked_by": [], "blocks": [{"id": "orch-m1", "state": "open", "title": "MVP"}]},
    });
    let input = trk_agent::prompt::PromptInput {
        issue: &issue,
        project_root: std::path::Path::new("/project"),
        config: &user_config(),
        issue_type: "code-change",
        type_description: "code change",
        agent: &a,
        tools: &tools,
        worktree: Some("/wt/orch-abc"),
        base: "develop",
        verify: "cargo test --workspace",
        actor: "agent:omp:u1",
        harness: Some("omp"),
    };
    let rules = trk_agent::prompt::instructions(&seed, &input).unwrap();
    assert!(rules.contains("Exit 75: not yet"), "{rules}");
    assert!(rules.contains("/wt/orch-abc"));
    assert!(rules.contains("landing branch is `develop`"), "{rules}");
    assert!(rules.contains("TRK_BASE (the landing branch)"), "{rules}");
    assert!(
        rules.contains(r#"[ "$(git branch --show-current)" = "$TRK_BASE" ]"#),
        "{rules}"
    );
    let b = trk_agent::prompt::briefing(&seed, &input, None).unwrap();
    assert!(b.contains("Claims need leases."), "{b}");
    assert!(b.contains("The waiting check exited with 1"), "{b}");
    assert!(b.contains("blocks orch-m1 (open): MVP"), "{b}");
    assert!(b.contains("priority P1"), "{b}");
    assert!(!b.contains("## Instructions"));
    let folded = trk_agent::prompt::briefing(&seed, &input, Some(&rules)).unwrap();
    assert!(folded.contains("## Instructions"));
}

#[test]
fn seed_waiting_rules_reflect_effective_check_settings() {
    let (_dir, seed) = seed();
    let agent = load_agent(&seed, "implementer").unwrap();
    let issue = trk_agent::check::sample_issue(&user_config());
    for max_wakes in [0, 3] {
        let mut config = user_config();
        config.check.interval = "1h".into();
        config.check.timeout = "20m".into();
        config.check.max_wakes = max_wakes;
        let tools = agent_tools(&agent, &config);
        let input = trk_agent::prompt::PromptInput {
            issue: &issue,
            project_root: std::path::Path::new("/project"),
            config: &config,
            issue_type: "code-change",
            type_description: "code change",
            agent: &agent,
            tools: &tools,
            worktree: Some("/wt/proj-abc"),
            base: "main",
            verify: "",
            actor: "agent:omp:u1",
            harness: Some("omp"),
        };
        let rules = trk_agent::prompt::instructions(&seed, &input).unwrap();
        assert!(rules.contains("`interval` (default `1h`)"), "{rules}");
        assert!(
            rules.contains("`[check] timeout` (configured `20m`)"),
            "{rules}"
        );
        if max_wakes == 0 {
            assert!(
                rules.contains("`[check] max_wakes = 0` disables waking"),
                "{rules}"
            );
            assert!(
                rules.contains("issue returns to open for a new session"),
                "{rules}"
            );
            assert!(!rules.contains("wakes this same session"), "{rules}");
        } else {
            assert!(
                rules.contains("`[check] max_wakes` (configured 3 per session)"),
                "{rules}"
            );
            assert!(rules.contains("wakes this same session"), "{rules}");
        }
    }
}

#[test]
fn launch_spec_for_omp() {
    let user = user_config();
    let profiles = harness::profiles(&user).unwrap();
    let (_dir, seed) = seed();
    let a = load_agent(&seed, "reviewer").unwrap();
    let mut env = BTreeMap::new();
    env.insert("TRK_DIR".to_string(), "/proj/.trk".to_string());
    let spec = harness::build(LaunchInput {
        issue: "orch-abc",
        agent: &a,
        harness: "omp",
        profile: &profiles["omp"],
        model: None,
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
    assert_eq!(spec.model.as_deref(), Some("openai-codex/gpt-6-luna"));
    assert!(
        spec.argv
            .windows(2)
            .any(|args| args == ["--model", "openai-codex/gpt-6-luna"])
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
    assert_eq!(v["mcpServers"]["trk"]["env"]["TRK_DIR"], "/proj/.trk");
    assert!(
        spec.shell_command
            .starts_with("cd /wt/orch-abc && exec env TRK_ACTOR=agent:omp:u1 omp")
    );
    assert!(spec.shell_command.ends_with("'Do it'\\''s thing'"));
    // trk run sessions do not see the trk skill.
    assert!(spec.files["/state/u1/omp.yml"].contains("ignoredSkills: [trk]"));
    assert!(
        spec.argv
            .windows(2)
            .any(|w| w[0] == "--config" && w[1] == "/state/u1/omp.yml")
    );
    assert!(!spec.send_briefing);
    assert!(profiles["opencode"].sends_briefing());
}
#[test]
fn claude_launch_passes_mcp_file_and_multiline_briefing_as_separate_arguments() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("worktree with spaces");
    let session = temp.path().join("session files");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&session).unwrap();
    let capture = temp.path().join("captured arguments");
    let fake_claude = temp.path().join("fake claude");
    std::fs::write(
        &fake_claude,
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$CAPTURE\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

    let user = user_config();
    let profiles = harness::profiles(&user).unwrap();
    let mut profile = profiles["claude-code"].clone();
    profile.command = fake_claude.to_string_lossy().into_owned();
    let (_dir, seed) = seed();
    let mut agent = load_agent(&seed, "implementer").unwrap();
    agent.harness.args.insert(
        "claude-code".into(),
        vec!["--label".into(), "issue {issue}: {model}".into()],
    );
    let model = "provider/model with 'quotes' $(echo nope)";
    profile
        .env
        .insert("SELECTED_MODEL".into(), "{model}".into());
    profile
        .files
        .insert("{session_dir}/model.txt".into(), "{model}".into());
    let briefing = "# issue: test\n\nQuotes: ' \" ` $HOME $(echo nope) ; & | < >\n\
                    Long detail: one two three four five six seven eight nine ten.\n";
    let spec = harness::build(LaunchInput {
        issue: "test-abc",
        agent: &agent,
        harness: "claude-code",
        profile: &profile,
        model: Some(model),
        actor: "agent:claude-code:test-session",
        on_behalf_of: None,
        terminal: "trk-test-abc",
        cwd: cwd.to_str().unwrap(),
        worktree: Some(cwd.to_str().unwrap()),
        session_dir: session.to_str().unwrap(),
        trk_bin: "/bin/trk",
        trk_env: BTreeMap::from([("TRK_DIR".to_string(), "/tmp/proj/.trk".to_string())]),
        briefing: briefing.to_string(),
        instructions: "rules".into(),
        tools: vec!["get_task".into(), "add_note".into()],
    })
    .unwrap();
    for (path, contents) in &spec.files {
        std::fs::write(path, contents).unwrap();
    }

    let output = Command::new("sh")
        .arg("-c")
        .arg(&spec.shell_command)
        .env("CAPTURE", &capture)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let args: Vec<String> = std::fs::read(&capture)
        .unwrap()
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
        .collect();
    assert_eq!(args[0], "--strict-mcp-config");
    assert_eq!(args[1], "--mcp-config");
    assert_eq!(args[2], format!("{}/mcp.json", session.display()));
    assert_eq!(
        &args[3..7],
        [
            "--model",
            model,
            "--label",
            &format!("issue test-abc: {model}"),
        ]
    );
    assert_eq!(args[7], briefing);
    assert_eq!(spec.env["SELECTED_MODEL"], model);
    assert_eq!(
        std::fs::read_to_string(session.join("model.txt")).unwrap(),
        model
    );
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&args[2]).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["trk"]["command"], "/bin/trk");
    assert_eq!(
        mcp["mcpServers"]["trk"]["args"],
        serde_json::json!([
            "mcp",
            "--issue",
            "test-abc",
            "--actor",
            "agent:claude-code:test-session",
            "--agent",
            "implementer"
        ])
    );
    assert_eq!(mcp["mcpServers"]["trk"]["env"]["TRK_DIR"], "/tmp/proj/.trk");
}

#[test]
fn builtin_levels_are_defined_by_profiles_not_a_fixed_list() {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in trk_core::seed::preset("minimal").unwrap() {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let agents = dir.path().join("agents");
    let cfg = ConfigDir::new(dir.path());
    let user = UserConfig {
        harness: toml::from_str(
            r#"
[enforcing]
command = "sh"
builtin_tools = { no-network = ["--offline"], plan-only = [] }
[preferred]
command = "sh"
"#,
        )
        .unwrap(),
        ..Default::default()
    };
    for level in [
        "default",
        "no-network",
        "plan-only",
        "undefined",
        "read-only",
        "none",
    ] {
        std::fs::write(
            agents.join("custom.toml"),
            format!(
                "name = 'custom'\n[harness]\nprefer = ['preferred']\nbuiltin_tools = '{level}'\n"
            ),
        )
        .unwrap();
        let diagnostics = trk_agent::check::check_agents(&cfg, &user);
        let errors: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.level == Level::Error)
            .collect();
        if matches!(level, "default" | "no-network" | "plan-only") {
            assert!(errors.is_empty(), "{level}: {diagnostics:#?}");
        } else {
            assert_eq!(errors.len(), 1, "{level}: {diagnostics:#?}");
            assert!(
                errors[0]
                    .message
                    .contains(&format!("builtin_tools '{level}'"))
            );
            assert!(
                errors[0]
                    .message
                    .contains("available levels: default, no-network, plan-only")
            );
        }
        assert_eq!(
            diagnostics.iter().any(|d| d.level == Level::Warning
                && d.message.contains("harness 'preferred' cannot enforce")),
            level != "default",
            "{level}: {diagnostics:#?}"
        );
    }
}
