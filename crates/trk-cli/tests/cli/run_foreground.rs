use super::*;

fn harness(e: &Env, command: &str, args: &[&str]) {
    let path = e.trk_dir().join("config.toml");
    let mut config = std::fs::read_to_string(&path).unwrap();
    config.push_str(
        &toml::to_string(&json!({
            "harness": { "foreground": {
                "command": command,
                "args": args,
                "builtin_tools": { "read-only": [] },
                "env": {
                    "TRK_DIR": "{{ trk_env.TRK_DIR }}",
                    "TRK_USER_DIR": "{{ trk_env.TRK_USER_DIR }}"
                }
            }}
        }))
        .unwrap(),
    );
    std::fs::write(path, config).unwrap();
}

fn shell(e: &Env, script: &str) {
    harness(
        e,
        "sh",
        &[
            "-ec",
            script,
            "foreground",
            "{{ trk_command }}",
            "{{ issue }}",
            e.repo().join("before-exit.json").to_str().unwrap(),
        ],
    );
}

#[test]
fn foreground_exit_releases_unfinished_claim_with_status() {
    for (command, args, code) in [
        ("sh", vec!["-c", "exit 0"], 0),
        ("sh", vec!["-c", "exit 23"], 23),
        ("missing-foreground-harness", vec![], 127),
    ] {
        let e = Env::new();
        harness(&e, command, &args);
        let id = e.create(&["Unfinished foreground session"]);
        let result = e.ok(&["run", &id, "--harness", "foreground", "--fg"]);
        assert_eq!(result["exit_code"], code);
        let issue = e.ok(&["show", &id]);
        assert_eq!(issue["state"], "open");
        assert!(issue["claim"].is_null());
        assert_eq!(issue["derived"]["ready"], true);
        let notes = issue["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["actor"], result["actor"]);
        assert_eq!(
            notes[0]["body"],
            format!("foreground session exited (exit status: {code})")
        );
        assert_eq!(e.ok(&["ready"])[0]["id"], id);
    }
}

#[test]
fn foreground_exit_preserves_ended_and_replacement_sessions() {
    for (action, state) in [
        ("close \"$2\"", "closed"),
        ("wait \"$2\" --reason 'needs review'", "waiting"),
        ("unclaim \"$2\" --message 'handed back'", "open"),
        (
            "unclaim \"$2\" --message 'handed back' >/dev/null; \"$1\" --json --as human:replacement@example.com claim \"$2\"",
            "claimed",
        ),
    ] {
        let e = Env::new();
        shell(
            &e,
            &format!("\"$1\" --json {action} >/dev/null; \"$1\" --json show \"$2\" > \"$3\""),
        );
        let id = e.create(&["Agent ends its foreground work"]);
        let result = e.ok(&["run", &id, "--harness", "foreground", "--fg"]);
        assert_eq!(result["exit_code"], 0, "{action}");
        let before: Value =
            serde_json::from_slice(&std::fs::read(e.repo().join("before-exit.json")).unwrap())
                .unwrap();
        let issue = e.ok(&["show", &id]);
        assert_eq!(issue["state"], state);
        assert_eq!(issue, before["data"]);
        if state == "claimed" {
            assert_eq!(issue["claim"]["actor"], "human:replacement@example.com");
        }
    }
}

#[test]
fn foreground_spawn_failure_releases_claim() {
    let e = Env::new();
    harness(&e, "sh", &[]);
    let id = e.create(&["Cannot start the foreground shell", "--type", "research"]);
    let output = e
        .cmd()
        .env("PATH", e.dir.path().join("no-executables"))
        .args(["--json", "run", &id, "--harness", "foreground", "--fg"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let issue = e.ok(&["show", &id]);
    assert_eq!(issue["state"], "open");
    assert_eq!(issue["derived"]["ready"], true);
    assert!(issue["claim"].is_null());
    assert!(
        issue["notes"][0]["body"]
            .as_str()
            .unwrap()
            .starts_with("launch failed:")
    );
}

#[test]
fn foreground_interrupt_releases_claim_after_harness_stops() {
    for signal in ["-INT", "-TERM", "-HUP"] {
        let e = Env::new();
        shell(&e, "echo ready > \"$3\"; exec sleep 30");
        let id = e.create(&["Interrupted foreground session"]);
        let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
        command
            .args(["--json", "run", &id, "--harness", "foreground", "--fg"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut runner = CheckRunner::spawn_command(&e, command);
        check_wait_until("foreground harness readiness", || {
            e.repo().join("before-exit.json").exists()
        });
        runner.signal(signal);
        assert!(runner.wait().success());
        let issue = e.ok(&["show", &id]);
        assert_eq!(issue["state"], "open");
        assert!(issue["claim"].is_null());
        assert_eq!(issue["derived"]["ready"], true);
        let note = issue["notes"][0]["body"].as_str().unwrap();
        assert!(
            note.starts_with("foreground session exited (signal:"),
            "{note}"
        );
    }
}
