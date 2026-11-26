use super::*;

#[test]
fn live_mcp_refuses_a_newer_database_without_mutating_the_issue() {
    let e = Env::new();
    let id = e.create(&["Preserve active session"]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let path = e.trk_dir().join("trk.db");
    let opts = trk_core::StoreOpts {
        now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
        prefix: "orch".into(),
        ..Default::default()
    };
    let server = trk_mcp::TrkServer::new(trk_mcp::McpArgs {
        db: path.clone(),
        config: trk_core::config::ConfigDir::with_user_dir(e.trk_dir(), None),
        project_root: e.repo(),
        user: trk_core::config::ConfigDir::with_user_dir(e.trk_dir(), None)
            .user_config()
            .unwrap(),
        opts: opts.clone(),
        issue: id.clone(),
        actor: spec["actor"].as_str().unwrap().into(),
        agent: "implementer".into(),
        base: "main".into(),
    })
    .unwrap();
    let (ok, before) = server.call_sync("get_task", json!({}));
    assert!(ok, "{before}");
    let store = trk_core::Store::open(&path, opts).unwrap();
    let current: u32 = store
        .conn()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    let future = current + 1;
    // A deterministic additive future migration while the MCP stays alive.
    store
        .conn()
        .execute_batch(&format!(
            "BEGIN IMMEDIATE;
             CREATE TABLE future_data (body TEXT NOT NULL);
             INSERT INTO future_data VALUES ('preserve future data');
             PRAGMA user_version = {future};
             COMMIT;"
        ))
        .unwrap();
    for (tool, args) in [
        ("get_task", json!({})),
        ("hand_back", json!({"note": "must not be written"})),
    ] {
        let (ok, error) = server.call_sync(tool, args);
        assert!(!ok, "{error}");
        assert_eq!(error["code"], "newer_database");
        assert_eq!(error["database"], path.to_str().unwrap());
        assert_eq!(error["database_version"], future);
        assert_eq!(error["supported_version"], current);
        let hint = error["hint"].as_str().unwrap();
        assert!(hint.contains("restart") && hint.contains("MCP") && hint.contains("get_task"));
    }
    e.err(&["show", &id], 1, "newer_database");
    let startup = e
        .cmd()
        .args([
            "mcp",
            "--issue",
            &id,
            "--actor",
            spec["actor"].as_str().unwrap(),
            "--agent",
            "implementer",
        ])
        .output()
        .unwrap();
    assert_eq!(startup.status.code(), Some(1));
    assert!(startup.stdout.is_empty(), "MCP stdout is protocol-only");
    let stderr = String::from_utf8(startup.stderr).unwrap();
    assert!(stderr.contains("SQLite migration version"), "{stderr}");
    assert!(
        stderr.contains("restart") && stderr.contains("MCP"),
        "{stderr}"
    );
    let after = store
        .read(None, |c| {
            c.show(
                &id,
                trk_core::view::With {
                    deps: true,
                    notes: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(after["claim"], before["claim"]);
    assert_eq!(after["state"], "claimed");
    assert_eq!(after["notes"], before["notes"]);
    assert_eq!(
        store
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        future
    );
    assert_eq!(
        store
            .conn()
            .query_row("SELECT body FROM future_data", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "preserve future data"
    );
}

/// Talk to `trk mcp` over stdio like a harness would: the session ends by
/// waiting on a check that merges its branch, and `trk check` lands it.
#[test]
fn mcp_server_over_stdio() {
    let e = Env::new();
    e.set_config("default_type = \"change\"", "default_type = \"task\"");
    std::fs::write(
        e.trk_dir().join("types/task.toml"),
        "name = \"task\"\ndescription = \"Project task\"\nagent = \"implementer\"\n",
    )
    .unwrap();
    std::fs::remove_file(e.trk_dir().join("types/change.toml")).unwrap();
    assert!(
        StdCommand::new("git")
            .current_dir(e.repo())
            .args(["branch", "-m", "develop"])
            .status()
            .unwrap()
            .success()
    );
    e.set_config("base = \"main\"", "base = \"develop\"");
    let id = e.create(&["Add a file", "-d", "Add hello.txt."]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let actor = spec["actor"].as_str().unwrap().to_string();
    let wt = PathBuf::from(spec["worktree"].as_str().unwrap());
    e.ok(&["note", &id, "keep it small", "--to-agent"]);
    let bin = assert_cmd::cargo::cargo_bin("trk");
    let mut child = StdCommand::new(bin)
        .args([
            "mcp",
            "--issue",
            &id,
            "--actor",
            &actor,
            "--agent",
            "implementer",
        ])
        // As the harness starts it: from anywhere, with TRK_DIR.
        .env("TRK_DIR", e.trk_dir())
        .env("HOME", e.dir.path().join("home"))
        .env("TRK_USER_DIR", e.user_dir())
        .env("TRK_NOW", "2026-11-20T10:00:00Z")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut n = 0;
    let mut req = |method: &str, params: Value| -> Value {
        n += 1;
        let msg = json!({"jsonrpc": "2.0", "id": n, "method": method, "params": params});
        writeln!(stdin, "{msg}").unwrap();
        loop {
            let mut line = String::new();
            assert!(stdout.read_line(&mut line).unwrap() > 0, "server closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(n) {
                if method == "initialize" {
                    writeln!(
                        stdin,
                        "{}",
                        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
                    )
                    .unwrap();
                }
                return v;
            }
        }
    };
    let init = req(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "trk");
    let rules = init["result"]["instructions"].as_str().unwrap();
    assert!(rules.contains("landing branch is `develop`"), "{rules}");
    assert!(
        rules.contains(&id) && rules.contains("Exit 75: not yet"),
        "{rules}"
    );
    let tools = req("tools/list", json!({}));
    let followup_tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "report_followup")
        .unwrap();
    assert_eq!(
        followup_tool["inputSchema"]["properties"]["type"]["enum"],
        json!(["bug", "ops", "research", "task"])
    );
    let type_description = followup_tool["inputSchema"]["properties"]["type"]["description"]
        .as_str()
        .unwrap();
    assert!(type_description.contains("default task"));
    assert!(type_description.contains("task: Project task"));
    let names: Vec<&str> = tools["result"]["tools"]
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
    let call =
        |req: &mut dyn FnMut(&str, Value) -> Value, name: &str, args: Value| -> (bool, Value) {
            let r = req("tools/call", json!({"name": name, "arguments": args}));
            let res = &r["result"];
            let text = res["content"][0]["text"].as_str().unwrap();
            (
                res["isError"] == json!(true),
                serde_json::from_str(text).unwrap(),
            )
        };
    let (err, v) = call(&mut req, "add_note", json!({"body": "working"}));
    assert!(!err);
    assert_eq!(v["messages_for_you"][0]["body"], "keep it small");
    assert_eq!(v["task"]["state"], "claimed");
    let (err, task) = call(&mut req, "get_task", json!({}));
    assert!(!err, "{task}");
    assert_eq!(task["type"], "task");
    assert_eq!(task["routing"]["agent"], "implementer");
    let (err, v) = call(&mut req, "close", json!({"reason": "merged", "note": "x"}));
    assert!(err);
    assert_eq!(v["error"]["code"], "invalid_field");
    let (err, v) = call(
        &mut req,
        "report_followup",
        json!({"title": "Also add bye.txt", "description": "Symmetry.", "type": "research"}),
    );
    assert!(!err, "{v}");
    let followup = v["result"]["created"].as_str().unwrap().to_string();
    let (err, defaulted) = call(
        &mut req,
        "report_followup",
        json!({"title": "Use the default type", "description": "Project policy."}),
    );
    assert!(!err, "{defaulted}");
    let defaulted = defaulted["result"]["created"].as_str().unwrap();
    assert_eq!(e.ok(&["show", defaulted])["type"], "task");
    let res = req("resources/read", json!({"uri": "trk://task"}));
    assert!(
        res["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&id)
    );
    // The "agent" does its work on the branch, then waits on a merge.
    std::fs::write(wt.join("hello.txt"), "hello\n").unwrap();
    for args in [
        vec!["add", "hello.txt"],
        vec![
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "hello",
        ],
    ] {
        assert!(
            StdCommand::new("git")
                .current_dir(&wt)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    let (err, v) = call(&mut req, "wait", json!({"check": "true"}));
    assert!(err, "a reason is required: {v}");
    let (err, v) = call(
        &mut req,
        "wait",
        json!({
            "reason": "merge into the base branch",
            "interval": "1m",
            "check": "cd \"$TRK_REPO\" && [ \"$(git branch --show-current)\" = \"$TRK_BASE\" ] && git -c user.email=t@e.st -c user.name=t merge --no-edit \"$TRK_BRANCH\"",
        }),
    );
    assert!(!err, "{v}");
    assert_eq!(v["waiting"], json!(id));
    let (err, v) = call(&mut req, "add_note", json!({"body": "after wait"}));
    assert!(!err, "{v}");
    assert_eq!(v["task"]["state"], "waiting");
    drop(stdin);
    child.wait().unwrap();

    let v = e.ok(&["show", &id]);
    assert_eq!(v["state"], "waiting");
    assert_eq!(v["waiting"]["interval"], 60);
    assert_eq!(v["waiting"]["cwd"], json!(wt.to_string_lossy()));
    let runs = e.ok(&["runs"]);
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["issue"], id);
    assert_eq!(runs[0]["state"], "waiting");
    assert!(!e.repo().join("hello.txt").exists());
    let r = e.ok(&["check"]);
    assert_eq!(r["results"][0]["result"], "passed", "{r:#}");
    assert!(e.repo().join("hello.txt").exists(), "the branch was merged");
    assert_eq!(e.ok(&["show", &id])["close_reason"], "done");
    let f = e.ok(&["show", &followup, "--with", "events"]);
    assert_eq!(f["created_by"], json!(actor));
    assert_eq!(f["type"], "research");
    assert_eq!(f["events"][0]["payload"]["followup_of"], json!(id));
}
