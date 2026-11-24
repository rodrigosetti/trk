use super::*;

fn write_file(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn shared_config(user: &Path) {
    for (path, content) in [
        (
            "config.toml",
            r#"
[user]
email = "shared@example.com"
[claims]
agent_lease = "2h"
[harness]
prefer = ["test"]
[harness.test]
command = "echo"
args = ["{{ briefing }}"]
[views.shared]
query = "state=open"
"#,
        ),
        (
            "agents/shared.toml",
            r#"
name = "shared"
description = "Shared agent"
instructions = "prompts/instructions.md.j2"
briefing = "prompts/briefing.md.j2"
inbox = true
[harness]
builtin_tools = "default"
"#,
        ),
        (
            "types/code-change.toml",
            r#"
name = "code-change"
agent = "shared"
description = "Shared type"
worktree = false
"#,
        ),
        (
            "prompts/instructions.md.j2",
            "USER INSTRUCTIONS {% include \"prompts/rules.md.j2\" %} base={{ base }}",
        ),
        (
            "prompts/briefing.md.j2",
            "USER BRIEFING {{ issue.id }} {% include \"prompts/rules.md.j2\" %}",
        ),
        ("prompts/rules.md.j2", "USER RULES"),
        (
            "prompts/_landing.merge.md.j2",
            include_str!("../../../../seed/prompts/_landing.merge.md.j2"),
        ),
    ] {
        write_file(user, path, content);
    }
}

fn command_data(mut command: Command, args: &[&str]) -> Value {
    let output = command.arg("--json").args(args).output().unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{args:?}: {envelope:#}");
    assert_eq!(envelope["ok"], true, "{envelope:#}");
    envelope["data"].clone()
}

fn pretty(e: &Env, args: &[&str]) -> String {
    let output = e
        .cmd()
        .args(["--format", "pretty", "--no-color"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn minimal_install_writes_only_project_settings_and_keeps_existing_files() {
    let e = Env::with_install(true);
    let config_path = e.trk_dir().join("config.toml");
    let text = std::fs::read_to_string(&config_path).unwrap();
    let config: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(config.len(), 3, "{text}");
    assert_eq!(config["user"]["email"].as_str(), Some("alice@example.com"));
    assert_eq!(config["issues"]["prefix"].as_str(), Some("orch"));
    assert_eq!(config["run"]["base"].as_str(), Some("main"));
    assert_eq!(config["run"].as_table().unwrap().len(), 1);
    for directory in ["agents", "types", "prompts"] {
        assert!(!e.trk_dir().join(directory).exists());
    }
    assert!(e.trk_dir().join("trk.db").is_file());
    assert_eq!(e.ok(&["whoami"])["prefix"], "orch");
    assert_eq!(e.ok(&["agents"]), json!([]));
    e.err(&["create", "Minimal project"], 4, "invalid_field");
    shared_config(&e.user_dir());
    assert!(e.create(&["Minimal project"]).starts_with("orch-"));
    let again = e.ok(&["install", "--minimal"]);
    assert_eq!(again["written"], json!([]));
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), text);

    // Minimal installation does not delete existing project-owned resources.
    write_file(&e.trk_dir(), "agents/custom.toml", "name = \"custom\"\n");
    e.ok(&["install", "--minimal", "--force", "--prefix", "local"]);
    assert_eq!(e.ok(&["whoami"])["prefix"], "local");
    assert!(e.trk_dir().join("agents/custom.toml").is_file());
    // The ordinary install remains complete and usable without a user layer.
    std::fs::remove_dir_all(e.user_dir()).unwrap();
    e.ok(&["install", "--force"]);
    for directory in ["agents", "types", "prompts"] {
        assert!(e.trk_dir().join(directory).is_dir());
    }
    let config: toml::Value =
        toml::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(config["harness"]["prefer"].as_array().unwrap().len(), 2);
    assert!(config["harness"]["omp"].is_table());
}

#[test]
fn discovery_prefers_explicit_then_xdg_then_home_without_touching_process_environment() {
    let e = Env::new();
    let explicit = e.user_dir();
    let xdg = e.dir.path().join("xdg");
    let home = e.dir.path().join("home");
    for (user, view) in [
        (explicit.clone(), "explicit"),
        (xdg.join("trk"), "xdg"),
        (home.join(".config/trk"), "home"),
    ] {
        write_file(
            &user,
            "config.toml",
            &format!("[views.{view}]\nquery = \"state=open\"\n"),
        );
    }
    let mut command = e.cmd();
    command.env("XDG_CONFIG_HOME", &xdg);
    assert_eq!(
        command_data(command, &["whoami"])["user_dir"],
        json!(explicit)
    );
    let mut command = e.cmd();
    command.env("XDG_CONFIG_HOME", &xdg);
    assert_eq!(command_data(command, &["list", "@explicit"]), json!([]));

    let mut command = e.cmd();
    command
        .env_remove("TRK_USER_DIR")
        .env("XDG_CONFIG_HOME", &xdg);
    assert_eq!(
        command_data(command, &["whoami"])["user_dir"],
        json!(xdg.join("trk"))
    );
    let mut command = e.cmd();
    command
        .env_remove("TRK_USER_DIR")
        .env("XDG_CONFIG_HOME", &xdg);
    assert_eq!(command_data(command, &["list", "@xdg"]), json!([]));

    let mut command = e.cmd();
    command
        .env_remove("TRK_USER_DIR")
        .env_remove("XDG_CONFIG_HOME");
    assert_eq!(
        command_data(command, &["whoami"])["user_dir"],
        json!(home.join(".config/trk"))
    );
    let mut command = e.cmd();
    command
        .env_remove("TRK_USER_DIR")
        .env_remove("XDG_CONFIG_HOME");
    assert_eq!(command_data(command, &["list", "@home"]), json!([]));

    let absent = e.dir.path().join("absent-user");
    let mut command = e.cmd();
    command
        .env("TRK_USER_DIR", &absent)
        .env("XDG_CONFIG_HOME", &xdg);
    assert_eq!(
        command_data(command, &["agents"]).as_array().unwrap().len(),
        3
    );
    assert!(!absent.exists());
    // An explicit missing directory does not silently use XDG or HOME.
    let output = e
        .cmd()
        .env("TRK_USER_DIR", &absent)
        .env("XDG_CONFIG_HOME", &xdg)
        .args(["--json", "list", "@xdg"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn layered_resources_report_origins_in_json_and_real_pretty_output() {
    let e = Env::with_install(true);
    shared_config(&e.user_dir());
    write_file(&e.trk_dir(), "prompts/rules.md.j2", "PROJECT RULES");
    let who = e.ok(&["whoami"]);
    assert_eq!(who["user_dir"], json!(e.user_dir()));
    assert!(pretty(&e, &["whoami"]).contains(&e.user_dir().display().to_string()));
    let agents = e.ok(&["agents"]);
    assert_eq!(agents.as_array().unwrap().len(), 1);
    let agent = &agents[0];
    assert_eq!(
        agent["source"],
        json!(e.user_dir().join("agents/shared.toml"))
    );
    assert_eq!(agent["source_layer"], "user");
    assert_eq!(agent["harness"], json!(["test"]));
    assert_eq!(agent["types"][0]["type_layer"], "user");
    assert_eq!(
        agent["templates"]["prompts/instructions.md.j2"]["layer"],
        "user"
    );
    assert_eq!(
        agent["templates"]["prompts/rules.md.j2"]["layer"],
        "project"
    );
    let id = e.create(&["Use shared resources"]);
    let spec = e.ok(&["run", &id, "--dry-run"]);
    assert_eq!(spec["harness"], "test");
    assert_eq!(spec["routing"]["agent_layer"], "user");
    assert_eq!(spec["routing"]["type_layer"], "user");
    assert_eq!(spec["templates"], agent["templates"]);
    assert!(
        spec["instructions"]
            .as_str()
            .unwrap()
            .contains("PROJECT RULES")
    );
    assert!(spec["briefing"].as_str().unwrap().contains("PROJECT RULES"));
    assert_eq!(e.ok(&["show", &id])["claim"], Value::Null);
    let checked = e.ok(&["agent", "check"]);
    assert_eq!(checked["resources"]["agents"][0]["source_layer"], "user");
    assert_eq!(checked["resources"]["types"][0]["source_layer"], "user");
    assert_eq!(checked["resources"]["templates"], agent["templates"]);
    for args in [
        vec!["agents"],
        vec!["agent", "check"],
        vec!["run", &id, "--harness", "test", "--dry-run"],
    ] {
        let output = pretty(&e, &args);
        for resource in [
            e.user_dir().join("agents/shared.toml"),
            e.user_dir().join("types/code-change.toml"),
            e.trk_dir().join("prompts/rules.md.j2"),
        ] {
            assert!(
                output.contains(&resource.display().to_string()),
                "{args:?}: {output}"
            );
        }
        assert!(
            output.contains("user") && output.contains("project"),
            "{output}"
        );
    }

    // Named resources are replaced whole; inherited templates remain per-file.
    write_file(
        &e.trk_dir(),
        "agents/shared.toml",
        "name = \"shared\"\ndescription = \"Project agent\"\ninstructions = \"prompts/instructions.md.j2\"\nbriefing = \"prompts/briefing.md.j2\"\n",
    );
    write_file(
        &e.trk_dir(),
        "types/code-change.toml",
        "agent = \"shared\"\ndescription = \"Project type\"\nworktree = false\n",
    );
    let spec = e.ok(&["run", &id, "--harness", "test", "--dry-run"]);
    assert_eq!(spec["routing"]["agent_layer"], "project");
    assert_eq!(spec["routing"]["type_layer"], "project");
    assert_eq!(
        spec["templates"]["prompts/instructions.md.j2"]["layer"],
        "user"
    );
}

struct Mcp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Mcp {
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "MCP server closed"
            );
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] == id {
                assert!(response.get("error").is_none(), "{response:#}");
                return response["result"].clone();
            }
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn cli_and_mcp_with_trk_dir_resolve_the_same_explicit_or_default_user_layer() {
    for explicit in [true, false] {
        let e = Env::with_install(true);
        let xdg = e.dir.path().join("xdg");
        let user = if explicit {
            e.user_dir()
        } else {
            xdg.join("trk")
        };
        shared_config(&user);
        write_file(&e.trk_dir(), "prompts/rules.md.j2", "PROJECT RULES");
        let cli = || {
            let mut command = e.cmd();
            command.env("XDG_CONFIG_HOME", &xdg);
            if !explicit {
                command.env_remove("TRK_USER_DIR");
            }
            command
        };
        let id = command_data(cli(), &["create", "Shared MCP session"])["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let spec = command_data(cli(), &["run", &id, "--harness", "test", "--print"]);
        let actor = spec["actor"].as_str().unwrap();
        let trk_server = &spec["mcp_servers"]["mcpServers"]["trk"];
        assert_eq!(
            trk_server["env"]["TRK_DIR"],
            json!(e.trk_dir().canonicalize().unwrap())
        );
        assert_eq!(trk_server["env"]["TRK_USER_DIR"], json!(user));
        let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
        command
            .current_dir(e.dir.path())
            .args(["mcp", "--issue", &id, "--actor", actor, "--agent", "shared"])
            .env("TRK_DIR", e.trk_dir())
            .env("HOME", e.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", &xdg)
            .env("TRK_NOW", "2026-11-20T10:00:00Z")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if explicit {
            command.env("TRK_USER_DIR", &user);
        } else {
            command.env_remove("TRK_USER_DIR");
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Mcp {
            child,
            stdin,
            stdout,
            next: 0,
        };
        let initialized = mcp.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
        assert_eq!(initialized["instructions"], spec["instructions"]);
        writeln!(
            mcp.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .unwrap();
        let tools = mcp.request("tools/list", json!({}));
        assert_eq!(tools["tools"].as_array().unwrap().len(), 6);
        let result = mcp.request("tools/call", json!({"name": "get_task", "arguments": {}}));
        assert_ne!(result["isError"], true, "{result:#}");
        let task: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        let shown = command_data(cli(), &["show", &id]);
        for field in [
            "type",
            "agent",
            "type_source",
            "agent_source",
            "type_layer",
            "agent_layer",
        ] {
            assert_eq!(task["routing"][field], spec["routing"][field], "{field}");
            assert_eq!(shown["routing"][field], task["routing"][field], "{field}");
        }
        assert_eq!(shown["claim"]["lease_until"], "2026-11-20T12:00:00Z");
    }
}

#[test]
fn dry_run_reports_only_templates_loaded_by_the_selected_harness_folding_mode() {
    let e = Env::with_install(true);
    shared_config(&e.user_dir());
    write_file(
        &e.user_dir(),
        "prompts/briefing.md.j2",
        "{% if fold_instructions %}{% include \"prompts/folded.md.j2\" %}{% else %}{% include \"prompts/separate.md.j2\" %}{% endif %}",
    );
    write_file(&e.user_dir(), "prompts/separate.md.j2", "SEPARATE");
    write_file(
        &e.trk_dir(),
        "prompts/folded.md.j2",
        "FOLDED {{ instructions }}",
    );
    let id = e.create(&["Report actual conditional templates"]);
    let separate = e.ok(&["run", &id, "--dry-run"]);
    assert_eq!(separate["briefing"], "SEPARATE");
    assert_eq!(
        separate["templates"]["prompts/separate.md.j2"]["layer"],
        "user"
    );
    assert!(separate["templates"].get("prompts/folded.md.j2").is_none());
    write_file(
        &e.user_dir(),
        "config.toml",
        "[harness]\nprefer = [\"test\"]\n[harness.test]\ncommand = \"echo\"\nargs = [\"{{ briefing }}\"]\nserver_instructions = false\n",
    );
    let folded = e.ok(&["run", &id, "--dry-run"]);
    assert!(
        folded["briefing"]
            .as_str()
            .unwrap()
            .contains("FOLDED USER INSTRUCTIONS")
    );
    assert_eq!(
        folded["templates"]["prompts/folded.md.j2"]["layer"],
        "project"
    );
    assert!(folded["templates"].get("prompts/separate.md.j2").is_none());
}
