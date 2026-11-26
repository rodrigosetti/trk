use super::*;

#[test]
fn create_run_launches_the_new_issue_and_preserves_it_on_launch_failure() {
    let e = Env::new();
    std::fs::write(
        e.trk_dir().join("config.toml"),
        r#"
[issues]
prefix = "orch"
[claims]
agent_lease = "30m"
[check]
interval = "5m"
timeout = "10m"
[run]
worktrees = ".trk/worktrees"
[launcher]
use = ["shell"]
[launcher.shell]
start = ["sh", "-c", "exec sh -c \"$1\"", "trk-launch", "{shell_command}"]
[harness.omp]
command = "sh"
args = ["-c", "printf '%s\\n' \"$1\"", "trk-harness", "{{ briefing }}"]
builtin_tools = { read-only = [] }
"#,
    )
    .unwrap();
    let older = e.create(&["Older urgent issue", "-p", "P0"]);
    let launched = e.ok(&[
        "create",
        "Review this change",
        "-d",
        "Check the edge cases.",
        "--type",
        "bug",
        "-p",
        "P3",
        "--run",
    ]);
    let id = launched["issue"].as_str().unwrap();
    let issue = e.ok(&["show", id]);
    assert_eq!(issue["title"], "Review this change");
    assert_eq!(issue["state"], "claimed");
    assert_eq!(issue["claim"]["actor"], launched["actor"]);
    assert_eq!(issue["claim"]["worktree"], launched["worktree"]);
    assert_eq!(issue["claim"]["agent"], "implementer");
    assert!(
        launched["launcher_output"]
            .as_str()
            .unwrap()
            .contains("Check the edge cases.")
    );
    assert_eq!(e.ok(&["show", &older])["state"], "open");

    e.set_config(
        r#"start = ["sh", "-c", "exec sh -c \"$1\"", "trk-launch", "{shell_command}"]"#,
        r#"start = ["sh", "-c", "echo unavailable >&2; exit 23"]"#,
    );
    let err = e.err(&["create", "Retry launch", "--run"], 1, "internal");
    let id = err["issue"].as_str().unwrap();
    let issue = e.ok(&["show", id]);
    assert_eq!(issue["title"], "Retry launch");
    assert_eq!(issue["state"], "open");
    assert!(issue["claim"].is_null());
    assert!(err["message"].as_str().unwrap().contains("unavailable"));
}

#[test]
fn create_run_respects_blockers_and_rejects_dry_run_without_creating() {
    let e = Env::new();
    let blocker = e.create(&["Blocker"]);
    let err = e.err(
        &["create", "Blocked", "--blocked-by", &blocker, "--run"],
        5,
        "blocked",
    );
    let id = err["issue"].as_str().unwrap();
    let issue = e.ok(&["show", id]);
    assert_eq!(issue["title"], "Blocked");
    assert_eq!(issue["state"], "open");
    assert_eq!(issue["deps"]["blocked_by"][0]["id"], blocker);
    assert!(!e.trk_dir().join("worktrees").exists());

    e.cmd()
        .args(["create", "Must not persist", "--run", "--dry-run"])
        .assert()
        .code(2);
    assert!(
        e.ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .all(|issue| issue["title"] != "Must not persist")
    );
}

#[test]
fn run_without_id_selects_ready_work_by_priority_and_age() {
    let e = Env::new();
    e.create(&["Older normal issue", "-p", "P2"]);
    let blocker = e.create(&["Blocker", "-p", "P3"]);
    e.create(&["Blocked urgent issue", "-p", "P0", "--blocked-by", &blocker]);
    let claimed = e.create(&["Claimed urgent issue", "-p", "P0"]);
    e.ok(&["claim", &claimed]);
    let closed = e.create(&["Closed urgent issue", "-p", "P0"]);
    e.ok(&["close", &closed]);
    let waiting = e.create(&["Waiting urgent issue", "-p", "P0"]);
    e.ok(&["claim", &waiting]);
    e.ok(&["wait", &waiting, "--reason", "Needs a decision"]);
    let older = e.at(
        "2026-11-20T11:00:00Z",
        &["create", "Older high-priority issue", "-p", "P1"],
    );
    let newer = e.at(
        "2026-11-20T12:00:00Z",
        &["create", "Newer high-priority issue", "-p", "P1"],
    );
    let id = older["id"].as_str().unwrap();

    let preview = e.ok(&["run", "--harness", "omp", "--dry-run"]);
    assert_eq!(preview["issue"], older["id"]);
    assert_eq!(e.ok(&["show", id])["state"], "open");
    assert!(!Path::new(preview["worktree"].as_str().unwrap()).exists());
    let launched = e.ok(&["run", "--harness", "omp", "--print"]);
    assert_eq!(launched["issue"], older["id"]);
    assert_eq!(e.ok(&["show", id])["state"], "claimed");
    assert!(Path::new(launched["worktree"].as_str().unwrap()).exists());
    assert_eq!(
        e.ok(&["run", "--harness", "omp", "--dry-run"])["issue"],
        newer["id"]
    );
}

#[test]
fn run_releases_claim_when_briefing_template_is_broken() {
    let e = Env::new();
    let id = e.create(&["Retry after fixing the briefing"]);
    let template = e.trk_dir().join("prompts/briefing.md.j2");
    let original = std::fs::read_to_string(&template).unwrap();
    std::fs::write(&template, "{% if %}").unwrap();

    let error = e.err(
        &["run", &id, "--harness", "omp", "--fg"],
        4,
        "config_invalid",
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("prompts/briefing.md.j2"),
        "{error:#}"
    );
    let issue = e.ok(&["show", &id]);
    assert_eq!(issue["state"], "open");
    assert!(issue["claim"].is_null());
    assert_eq!(issue["derived"]["ready"], true);
    assert_eq!(
        issue["notes"].as_array().unwrap().last().unwrap()["body"],
        format!("launch failed: {}", error["message"].as_str().unwrap())
    );
    assert_eq!(e.ok(&["ready"])[0]["id"], id);

    std::fs::write(&template, original).unwrap();
    assert_eq!(e.ok(&["run", "--harness", "omp", "--print"])["issue"], id);
}

#[test]
fn agent_preview_and_run_dry_run() {
    let e = Env::new();
    let list = e.ok(&["agent", "list"]);
    let names: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["implementer", "operator", "researcher"]);
    assert!(list[0].get("default").is_none());
    let p = e.ok(&["agent", "preview", "implementer"]);
    assert!(
        p["instructions"]
            .as_str()
            .unwrap()
            .contains("implementer agent")
    );
    let names: Vec<&str> = p["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
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
    let id = e.create(&["Fix", "-d", "The thing is broken."]);
    e.ok(&["note", &id, "The waiting check exited with 1"]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--dry-run"]);
    assert_eq!(spec["agent"], "implementer");
    assert_eq!(spec["routing"]["worktree"], true);
    assert_eq!(spec["argv"][0], "omp");
    assert!(
        spec["worktree"]
            .as_str()
            .unwrap()
            .ends_with(&format!("worktrees/{id}"))
    );
    let briefing = spec["briefing"].as_str().unwrap();
    assert!(briefing.contains("The thing is broken."), "{briefing}");
    // A returning issue explains itself through its notes.
    assert!(
        briefing.contains("The waiting check exited with 1"),
        "{briefing}"
    );
    assert!(spec["actor"].as_str().unwrap().starts_with("agent:omp:"));
    // Nothing was claimed or created.
    assert_eq!(e.ok(&["show", &id])["claim"], Value::Null);
    assert!(!Path::new(spec["worktree"].as_str().unwrap()).exists());
    // The issue type selects its agent; --agent overrides that routing.
    e.ok(&["edit", &id, "--type", "bug"]);
    assert_eq!(
        e.ok(&["run", &id, "--harness", "omp", "--dry-run"])["agent"],
        "implementer"
    );
    assert_eq!(
        e.ok(&[
            "run",
            &id,
            "--harness",
            "omp",
            "--agent",
            "researcher",
            "--dry-run"
        ])["agent"],
        "researcher"
    );
    e.ok(&["edit", &id, "--type", "change"]);
    // --print claims, creates the worktree on a branch named after the issue,
    // and writes the files.
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let wt = PathBuf::from(spec["worktree"].as_str().unwrap());
    assert!(wt.join(".omp/mcp.json").exists());
    let exclude = std::fs::read_to_string(e.repo().join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("/.omp/mcp.json"));
    let branch = StdCommand::new("git")
        .current_dir(&wt)
        .args(["branch", "--show-current"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), id);
    let v = e.ok(&["show", &id]);
    assert_eq!(v["claim"]["agent"], "implementer");
    assert_eq!(v["state"], "claimed");
    assert_eq!(e.ok(&["runs"])[0]["issue"], json!(id));
    e.err(
        &["run", &id, "--harness", "omp", "--dry-run"],
        5,
        "claimed_by_other",
    );
    // An issue that is not open cannot be run.
    let done = e.create(&["Done"]);
    e.ok(&["close", &done]);
    e.err(&["run", &done, "--dry-run"], 4, "invalid_state");
}

#[test]
fn run_refuses_harnesses_without_the_agents_builtin_level_before_claiming() {
    let e = Env::new();
    std::fs::write(
        e.trk_dir().join("config.toml"),
        r#"
[issues]
prefix = "orch"
[claims]
agent_lease = "30m"
[check]
interval = "5m"
timeout = "10m"
[run]
worktrees = ".trk/worktrees"
[harness.unsupported]
command = "sh"
[harness.enforcing]
command = "sh"
args = ["{builtin_tools_args}"]
builtin_tools = { no-network = ["--offline"] }
"#,
    )
    .unwrap();
    std::fs::write(
        e.trk_dir().join("agents/implementer.toml"),
        "name = 'implementer'\n[harness]\nprefer = ['unsupported']\nbuiltin_tools = 'no-network'\n",
    )
    .unwrap();
    let id = e.create(&["Enforce custom tool policy"]);
    for mode in ["--dry-run", "--print", "--fg"] {
        for explicit in [false, true] {
            let mut args = vec!["run", &id, mode];
            if explicit {
                args.extend(["--harness", "unsupported"]);
            }
            let error = e.err(&args, 4, "config_invalid");
            assert!(
                error["message"].as_str().unwrap().contains(
                    "harness 'unsupported' cannot enforce builtin_tools = \"no-network\""
                ),
                "{error:#}"
            );
            let hint = error["hint"].as_str().unwrap();
            assert!(hint.contains("builtin_tools.no-network"), "{hint}");
            assert!(hint.contains("--harness"), "{hint}");
            let issue = e.ok(&["show", &id]);
            assert_eq!(issue["state"], "open");
            assert!(issue["claim"].is_null());
            assert!(!e.trk_dir().join("worktrees").join(&id).exists());
        }
    }
    let spec = e.ok(&["run", &id, "--harness", "enforcing", "--print"]);
    assert_eq!(spec["argv"], json!(["sh", "--offline"]));
    assert_eq!(e.ok(&["show", &id])["state"], "claimed");
}

#[test]
fn codex_dry_run_passes_every_mcp_server_as_toml_overrides() {
    let e = Env::new();
    std::fs::write(
        e.trk_dir().join("agents/extra-mcp.toml"),
        r#"
name = "extra-mcp"
[harness]
prefer = ["codex"]
[[mcp]]
name = "extra-search"
command = '/path with spaces/quote"and\slash'
args = ["--label", "quotes ' \" \\ \n {{ issue }}", ""]
[mcp.env]
TOKEN = "${env:TRK_TEST_CODEX_TOKEN}"
"key.with.dots" = "literal {{ issue }}"
[[mcp]]
name = "empty"
command = "empty-server"
"#,
    )
    .unwrap();
    let id = e.create(&["Codex MCP overrides"]);
    let token = "secret ' \" \\ \n ${env:DO_NOT_EXPAND} {{ issue }}";
    let output = e
        .cmd()
        .env("TRK_TEST_CODEX_TOKEN", token)
        .args([
            "--json",
            "run",
            &id,
            "--agent",
            "extra-mcp",
            "--harness",
            "codex",
            "--dry-run",
        ])
        .output()
        .unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{envelope:#}");
    let spec = &envelope["data"];
    assert_eq!(spec["argv"][0], "codex");
    assert_eq!(
        spec["argv"].as_array().unwrap().last().unwrap(),
        &spec["briefing"]
    );
    let servers = &spec["mcp_servers"]["mcpServers"];
    assert_eq!(servers["extra-search"]["env"]["TOKEN"], token);
    let argv = spec["argv"].as_array().unwrap();
    let overrides = &argv[1..argv.len() - 1];
    assert_eq!(overrides.len(), 18, "{argv:?}");
    for (server_index, server) in ["trk", "extra-search", "empty"].iter().enumerate() {
        for (field_index, field) in ["command", "args", "env"].iter().enumerate() {
            let index = 6 * server_index + 2 * field_index;
            assert_eq!(overrides[index], "-c");
            let prefix = format!("mcp_servers.{server}.{field}=");
            let value = overrides[index + 1]
                .as_str()
                .unwrap()
                .strip_prefix(&prefix)
                .unwrap();
            let parsed: toml::Table = toml::from_str(&format!("value = {value}")).unwrap();
            assert_eq!(
                serde_json::to_value(&parsed["value"]).unwrap(),
                servers[server][field],
                "{server}.{field}"
            );
        }
    }
    assert_eq!(e.ok(&["show", &id])["claim"], Value::Null);
}

#[test]
fn run_resolves_profile_agent_per_harness_and_cli_models_and_args() {
    let e = Env::new();
    let id = e.create(&["Review models", "--type", "bug"]);
    e.set_config(
        "model   = \"openai-codex/gpt-6-luna\"",
        "model   = \"profile-model\"",
    );
    // Model resolution across all profiles uses an unrestricted agent.
    let agent_path = e.trk_dir().join("agents/implementer.toml");
    let original = std::fs::read_to_string(&agent_path).unwrap();
    let check = |harness: &str, model: Option<&str>, extra: &[&str], cli: Option<&str>| {
        let mut args = vec!["run", &id, "--harness", harness, "--dry-run"];
        if let Some(model) = cli {
            args.extend(["--model", model]);
        }
        let spec = e.ok(&args);
        assert_eq!(spec["model"], json!(model), "{spec:#}");
        let argv: Vec<&str> = spec["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();
        match model {
            Some(model) => {
                let pos = argv.iter().position(|arg| *arg == "--model").unwrap();
                assert_eq!(argv[pos + 1], model, "{argv:?}");
                assert_eq!(argv.iter().filter(|arg| **arg == "--model").count(), 1);
            }
            None => assert!(!argv.contains(&"--model"), "{argv:?}"),
        }
        let end = if harness == "opencode" {
            assert_eq!(spec["send_briefing"], true);
            argv.len()
        } else {
            assert_eq!(argv.last().copied(), spec["briefing"].as_str());
            argv.len() - 1
        };
        assert_eq!(&argv[end - extra.len()..end], extra);
    };

    // No resolved model means no model flag or empty model argument.
    check("omp", Some("profile-model"), &[], None);
    for harness in ["claude-code", "codex", "opencode", "pi"] {
        check(harness, None, &[], None);
        check(harness, Some("one-run"), &[], Some("one-run"));
    }
    let extra = r#"
args = { omp = ["--label", "review {{ issue }}: {{ model }}"], opencode = ["--label", "{{ model }}"] }
"#;
    std::fs::write(
        &agent_path,
        format!("{original}\nmodel = \"agent-model\"\n{extra}"),
    )
    .unwrap();
    let label = format!("review {id}: agent-model");
    check("omp", Some("agent-model"), &["--label", &label], None);
    check("claude-code", Some("agent-model"), &[], None);

    std::fs::write(
        &agent_path,
        format!(
            "{original}\nmodel = {{ omp = \"omp-model\", claude-code = \"claude-model\" }}\n{extra}"
        ),
    )
    .unwrap();
    let label = format!("review {id}: omp-model");
    check("omp", Some("omp-model"), &["--label", &label], None);
    check("claude-code", Some("claude-model"), &[], None);
    check("codex", None, &[], None);
    check("opencode", None, &["--label", ""], None);
    check(
        "opencode",
        Some("sent-model"),
        &["--label", "sent-model"],
        Some("sent-model"),
    );
    let label = format!("review {id}: cli-model");
    check(
        "omp",
        Some("cli-model"),
        &["--label", &label],
        Some("cli-model"),
    );
    let list = e.ok(&["agents"]);
    let implementer = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "implementer")
        .unwrap();
    assert_eq!(
        implementer["model"],
        json!({"omp": "omp-model", "claude-code": "claude-model"})
    );
    assert_eq!(implementer["models"]["omp"], "omp-model");
    assert_eq!(implementer["models"]["codex"], Value::Null);
    assert_eq!(
        implementer["args"]["omp"],
        json!(["--label", "review {{ issue }}: {{ model }}"])
    );
    let pretty = e
        .cmd()
        .args(["--format", "pretty", "agents"])
        .output()
        .unwrap();
    assert!(pretty.status.success());
    let text = String::from_utf8(pretty.stdout).unwrap();
    assert!(text.contains("omp=omp-model"), "{text}");
    assert!(text.contains("claude-code=claude-model"), "{text}");

    // A model table lacking the selected harness falls back to the profile.
    std::fs::write(
        &agent_path,
        format!("{original}\nmodel = {{ pi = \"pi-model\" }}"),
    )
    .unwrap();
    check("omp", Some("profile-model"), &[], None);
    assert_eq!(e.ok(&["show", &id])["claim"], Value::Null);
}

#[test]
fn types_without_worktrees_run_and_check_in_the_project_root() {
    let e = Env::new();
    let root = std::fs::canonicalize(e.repo()).unwrap();
    let linked = e.dir.path().join("linked");
    assert!(
        StdCommand::new("git")
            .current_dir(&root)
            .args(["worktree", "add", "-q", "-b", "side"])
            .arg(&linked)
            .status()
            .unwrap()
            .success()
    );
    e.set_config("base = \"main\"", "base = \"side\"");
    let config = e.trk_dir().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        config,
        format!(
            "{text}\n\
             [harness.cwd-proof]\n\
             command = 'sh'\n\
             args = ['-c', 'pwd > observed-cwd']\n\
             builtin_tools = {{ read-only = [] }}\n\
             files = {{ '.omp/mcp.json' = '{{{{ mcp_servers | tojson }}}}' }}\n"
        ),
    )
    .unwrap();

    std::fs::write(
        e.trk_dir().join("types/advice.toml"),
        "description = 'Custom advice'\nagent = 'researcher'\nworktree = false\n",
    )
    .unwrap();
    for issue_type in ["research", "advice"] {
        let id = e.create(&["Read the current checkout", "--type", issue_type]);
        assert_eq!(e.ok(&["show", &id])["routing"]["worktree"], false);
        let preview = e.ok(&["run", &id, "--harness", "cwd-proof", "--dry-run"]);
        assert_eq!(preview["routing"]["worktree"], false);
        assert!(preview["worktree"].is_null());
        assert_eq!(preview["cwd"], json!(root));
        assert!(e.ok(&["show", &id])["claim"].is_null());

        // Launch from a linked checkout: a read-only type still sees the primary.
        let out = e
            .cmd()
            .current_dir(&linked)
            .args(["--json", "run", &id, "--harness", "cwd-proof", "--fg"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let launched: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(launched["data"]["exit_code"], 0);
        assert_eq!(
            std::fs::read_to_string(root.join("observed-cwd"))
                .unwrap()
                .trim(),
            root.to_str().unwrap()
        );
        let issue = e.ok(&["show", &id]);
        assert_eq!(issue["state"], "open");
        assert!(issue["claim"].is_null());
        assert!(!e.trk_dir().join("worktrees").join(&id).exists());
        let branch = StdCommand::new("git")
            .current_dir(&root)
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/heads/{id}"))
            .status()
            .unwrap();
        assert_eq!(branch.code(), Some(1));
        let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains("/.omp/mcp.json"));

        let spec = e.ok(&["run", &id, "--harness", "cwd-proof", "--print"]);
        let claim = e.ok(&["show", &id])["claim"].clone();
        assert!(claim["worktree"].is_null());
        let actor = spec["actor"].as_str().unwrap();
        e.ok(&[
            "--as",
            actor,
            "wait",
            &id,
            "--reason",
            "verify current checkout",
            "--check",
            "test \"$(pwd)\" = \"$TRK_REPO\" && test \"$TRK_WORKTREE\" = \"$TRK_REPO\" && test \"$TRK_BRANCH\" = main && test \"$TRK_BASE\" = side",
        ]);
        let check = e.ok(&["check", &id]);
        assert_eq!(check["results"][0]["result"], "passed", "{check:#}");
        assert_eq!(e.ok(&["show", &id])["state"], "closed");
    }
}

#[test]
fn launcher_configuration_errors_name_the_invalid_setting() {
    for (from, to, key) in [
        ("use = [\"herdr\"]", "", "launcher.use"),
        ("use = [\"herdr\"]", "use = \"herdr\"", "launcher.use"),
        ("use = [\"herdr\"]", "use = [\"\"]", "launcher profile name"),
        (
            "use = [\"herdr\"]",
            "use = [\"missing\"]",
            "launcher.missing",
        ),
        (
            "use = [\"herdr\"]",
            "use = [\"empty\"]\n[launcher.empty]",
            "launcher.empty.start",
        ),
        (
            "driver = \"herdr\"",
            "driver = \"unknown\"",
            "launcher.herdr.driver",
        ),
        ("send_delay = 5", "send_delay = -1", "launcher.send_delay"),
    ] {
        let e = Env::new();
        e.set_config(from, to);
        let id = e.create(&["Invalid launcher"]);
        let (_, response) = e.run(&["run", &id, "--harness", "omp"]);
        assert_eq!(response["error"]["code"], "config_invalid", "{response}");
        assert!(
            response["error"]["message"].as_str().unwrap().contains(key),
            "{key}: {response}"
        );
        assert_eq!(e.ok(&["show", &id])["state"], "open");
    }
}

#[test]
fn printed_launches_do_not_record_an_owned_terminal_or_require_a_launcher() {
    let e = Env::new();
    e.set_config("use = [\"herdr\"]", "");
    let id = e.create(&["Print without a terminal"]);
    e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let issue = e.ok(&["show", &id]);
    assert!(issue["claim"]["terminal"].is_null(), "{issue}");
    assert!(issue["claim"]["launcher"].is_null(), "{issue}");
    let runs = e.ok(&["runs"]);
    assert_eq!(runs[0]["issue"], id);
    assert!(runs[0]["session_alive"].is_null(), "{runs}");
    assert!(e.ok(&["reap"]).as_array().unwrap().is_empty());
}
