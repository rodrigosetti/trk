use super::*;

fn response(mut command: Command, args: &[&str]) -> (i32, Value) {
    let output = command.arg("--json").args(args).output().unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{args:?}: invalid JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (output.status.code().unwrap_or(-1), value)
}

fn success(command: Command, args: &[&str]) -> Value {
    let (code, value) = response(command, args);
    assert_eq!(code, 0, "{args:?}: {value:#}");
    assert_eq!(value["ok"], true, "{args:?}: {value:#}");
    assert_eq!(value["schema_version"], 4);
    value["data"].clone()
}

fn failure(command: Command, args: &[&str], exit: i32, code: &str) -> Value {
    let (actual, value) = response(command, args);
    assert_eq!(actual, exit, "{args:?}: {value:#}");
    assert_eq!(value["ok"], false, "{args:?}: {value:#}");
    assert_eq!(value["error"]["code"], code, "{args:?}: {value:#}");
    value["error"].clone()
}

fn store(ui: &WaitingUi) -> trk_core::Store {
    trk_core::Store::open(
        &ui.e.trk_dir().join("trk.db"),
        trk_core::StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            ..Default::default()
        },
    )
    .unwrap()
}

fn original_claim(ui: &WaitingUi, id: &str, terminal: Option<&str>) -> trk_core::model::Claim {
    let hostname = StdCommand::new("hostname").output().unwrap();
    assert!(hostname.status.success());
    let host = String::from_utf8(hostname.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let actor = format!("agent:omp:{id}-original");
    store(ui)
        .write(&actor, false, |c| {
            let claim = c.claim(
                id,
                trk_core::ops::ClaimOpts {
                    lease: Some(jiff::SignedDuration::from_mins(1)),
                    worktree: Some(ui.e.repo().to_string_lossy().into_owned()),
                    host: Some(host.clone()),
                    agent: Some("implementer".into()),
                    terminal: terminal.map(str::to_owned),
                    ..Default::default()
                },
            )?;
            c.patch_actor_meta(
                &actor,
                json!({"mcp_pid": std::process::id(), "mcp_host": host}),
            )?;
            Ok(claim)
        })
        .unwrap()
}

fn leave_waiting(ui: &WaitingUi, id: &str, check: Option<&str>) {
    let mut args = vec!["wait", id, "--force", "--reason", "human decision"];
    if let Some(check) = check {
        args.extend(["--check", check]);
    }
    ui.e.ok(&args);
}

fn recovery_hint(error: &Value, id: &str) {
    let hint = error["hint"].as_str().unwrap();
    assert!(hint.contains(&format!("trk reopen {id} -m")), "{error:#}");
    assert!(hint.contains(&format!("trk run {id}")), "{error:#}");
}

#[test]
fn human_resume_restores_the_original_claim_with_a_fresh_lease() {
    for check in [None, Some("exit 75")] {
        let ui = WaitingUi::new();
        ui.e.set_config("max_wakes = 5", "max_wakes = 0");
        let id = ui.e.create(&["Human-authorized continuation"]);
        let original = original_claim(&ui, &id, Some("exact-origin"));
        ui.e.set_config(
            "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
            r#"send = ["sh", "-c", "\"$TRK_TEST_BINARY\" --json show \"$TRK_TEST_ISSUE\" > \"$TRK_TEST_UI/at-send.json\" || exit; printf '%s\\n%s\\n' \"$0\" \"$1\" >> \"$TRK_TEST_UI/messages\"", "{name}", "{text}"]"#,
        );
        leave_waiting(&ui, &id, check);
        let before = ui.e.ok(&["show", &id, "--with", "all"]);
        assert_eq!(before["state"], "waiting");
        assert_eq!(before["claim"], Value::Null);
        let mut command = ui.cmd();
        command
            .env("TRK_NOW", "2026-11-20T10:05:00Z")
            .env("TRK_TEST_BINARY", env!("CARGO_BIN_EXE_trk"))
            .env("TRK_TEST_ISSUE", &id);
        let result = success(
            command,
            &["resume", &id, "-m", "  Continue with option B.  "],
        );
        assert_eq!(result["issue"], id);
        assert_eq!(result["actor"], original.actor);
        assert_eq!(result["terminal"], "exact-origin");
        assert_eq!(result["resumed_from"], original.id);
        assert!(result["claim"].as_i64().unwrap() > original.id);
        assert_eq!(result["message"], "Continue with option B.");
        assert_eq!(result["delivered"], true);
        let messages = ui.text("messages");
        assert!(messages.starts_with("exact-origin\n"), "{messages}");
        assert!(
            messages.ends_with("\n\nContinue with option B.\n"),
            "{messages}"
        );
        let at_send: Value = serde_json::from_str(&ui.text("at-send.json")).unwrap();
        assert_eq!(at_send["data"]["state"], "claimed");
        assert_eq!(at_send["data"]["claim"]["actor"], original.actor);
        assert_eq!(
            at_send["data"]["claim"]["lease_until"],
            "2026-11-20T10:35:00Z"
        );
        assert_eq!(ui.text("renames"), "");

        let issue =
            ui.e.at("2026-11-20T10:05:00Z", &["show", &id, "--with", "all"]);
        assert_eq!(issue["state"], "claimed");
        assert_eq!(issue["waiting"], Value::Null);
        assert_eq!(issue["wake_count"], before["wake_count"]);
        let claim = &issue["claim"];
        assert_eq!(claim["actor"], original.actor);
        assert_eq!(claim["terminal"], json!(original.terminal));
        assert_eq!(claim["worktree"], json!(original.worktree));
        assert_eq!(claim["host"], json!(original.host));
        assert_eq!(claim["agent"], json!(original.agent));
        assert_eq!(claim["claimed_at"], "2026-11-20T10:05:00Z");
        assert_eq!(claim["lease_until"], "2026-11-20T10:35:00Z");
        let claims = issue["claims"].as_array().unwrap();
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[0], before["claims"][0]);
        assert_eq!(claims[1]["released_at"], Value::Null);
        assert!(
            issue["notes"].as_array().unwrap().iter().any(|note| {
                note["actor"] == "human:alice@example.com"
                    && note["body"]
                        .as_str()
                        .unwrap()
                        .contains("Continue with option B.")
            }),
            "{issue:#}"
        );
        assert!(
            issue["events"].as_array().unwrap().iter().any(|event| {
                event["op"] == "claim"
                    && event["actor"] == "human:alice@example.com"
                    && event["payload"]["resumed_from"] == original.id
            }),
            "{issue:#}"
        );
        assert!(
            issue["events"].as_array().unwrap().iter().any(|event| {
                event["op"] == "note"
                    && event["actor"] == "human:alice@example.com"
                    && event["payload"]["resume"]["result"] == "delivered"
                    && event["payload"]["resume"]["claim"] == result["claim"]
            }),
            "{issue:#}"
        );
        let runs = ui.e.at("2026-11-20T10:05:00Z", &["runs"]);
        assert_eq!(runs.as_array().unwrap().len(), 1);
        assert_eq!(runs[0]["state"], "claimed");
        assert_eq!(runs[0]["actor"], original.actor);
        assert_eq!(runs[0]["terminal"], "exact-origin");
    }
}

#[test]
fn human_resume_preserves_a_nonzero_exhausted_automatic_wake_budget() {
    let ui = WaitingUi::new();
    ui.e.set_config("max_wakes = 5", "max_wakes = 1");
    let id = ui.waiting("exit 1", true);
    assert_eq!(ui.check(&id, "")["result"], "woken");
    assert_eq!(ui.e.ok(&["show", &id])["wake_count"], 1);
    leave_waiting(&ui, &id, Some("exit 1"));
    let before = ui.e.ok(&["show", &id, "--with", "claims"]);
    let delivered = ui.text("messages");
    success(
        ui.cmd(),
        &["resume", &id, "-m", "One more human-authorized attempt"],
    );
    let after = ui.e.ok(&["show", &id, "--with", "claims"]);
    assert_eq!(after["state"], "claimed");
    assert_eq!(after["wake_count"], 1);
    assert_eq!(after["claims"].as_array().unwrap().len(), 3);
    assert_eq!(after["claim"]["actor"], before["claims"][1]["actor"]);
    let messages = ui.text("messages");
    assert!(
        messages.starts_with(&format!("{delivered}exact-origin\n")),
        "{messages}"
    );
    assert!(
        messages.ends_with("\n\nOne more human-authorized attempt\n"),
        "{messages}"
    );
}

#[test]
fn agents_cannot_authorize_resume() {
    let ui = WaitingUi::new();
    let id = ui.waiting("exit 75", true);
    let before = ui.e.ok(&["show", &id, "--with", "all"]);
    let mut command = ui.cmd();
    command.env("TRK_ACTOR", format!("agent:omp:{id}-exact-origin"));
    let error = failure(
        command,
        &["resume", &id, "-m", "Continue"],
        5,
        "not_permitted",
    );
    let hint = error["hint"].as_str().unwrap().to_lowercase();
    assert!(hint.contains("human"), "{error:#}");
    for impersonation in ["--as", "trk_actor", "human:"] {
        assert!(!hint.contains(impersonation), "{error:#}");
    }
    assert_eq!(ui.e.ok(&["show", &id, "--with", "all"]), before);
    assert_eq!(ui.text("messages"), "");
}

#[test]
fn resume_requires_a_nonempty_message() {
    let ui = WaitingUi::new();
    let id = ui.waiting("exit 75", true);
    let before = ui.e.ok(&["show", &id, "--with", "all"]);
    for args in [
        vec!["resume", id.as_str()],
        vec!["resume", id.as_str(), "-m"],
        vec!["resume", id.as_str(), "-m", ""],
        vec!["resume", id.as_str(), "-m", " \t\n "],
    ] {
        failure(ui.cmd(), &args, 2, "bad_args");
    }
    assert_eq!(ui.e.ok(&["show", &id, "--with", "all"]), before);
    assert_eq!(ui.text("messages"), "");
}

#[test]
fn resume_refuses_nonwaiting_and_replacement_claims() {
    for state in ["open", "closed", "replacement"] {
        let ui = WaitingUi::new();
        let id = ui.waiting("exit 75", true);
        match state {
            "closed" => {
                ui.e.ok(&["close", &id]);
            }
            _ => {
                ui.e.ok(&["reopen", &id]);
                if state == "replacement" {
                    ui.claim(&id, "new-session", true);
                }
            }
        }
        let before = ui.e.ok(&["show", &id, "--with", "all"]);
        failure(
            ui.cmd(),
            &["resume", &id, "-m", "Do not resurrect"],
            4,
            "invalid_state",
        );
        assert_eq!(ui.e.ok(&["show", &id, "--with", "all"]), before, "{state}");
        assert_eq!(ui.text("messages"), "", "{state}");
    }
}

#[test]
fn resume_refuses_unavailable_original_sessions_without_replacement() {
    for mode in [
        "absent",
        "dead",
        "remote",
        "missing",
        "unavailable",
        "no-terminal",
        "no-send",
    ] {
        let ui = WaitingUi::new();
        let id = ui.e.create(&["Unavailable waiting session"]);
        let terminal = (mode != "no-terminal").then_some("exact-origin");
        let original = original_claim(&ui, &id, terminal);
        let patch = match mode {
            "dead" => json!({"mcp_exited_at": "2026-11-20T10:00:00Z"}),
            "absent" => json!({"mcp_pid": null, "mcp_host": null}),
            "remote" => json!({"mcp_host": "unreachable.invalid"}),
            _ => json!({}),
        };
        store(&ui)
            .write("human:alice@example.com", false, |c| {
                c.patch_actor_meta(&original.actor, patch)
            })
            .unwrap();
        leave_waiting(&ui, &id, None);
        if mode == "no-send" {
            ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "",
            );
        }
        let before = ui.e.ok(&["show", &id, "--with", "all"]);
        let mut command = ui.cmd();
        command.env("TRK_TEST_UI_FAILURE", mode);
        let error = failure(
            command,
            &["resume", &id, "-m", "Continue"],
            4,
            "invalid_state",
        );
        recovery_hint(&error, &id);
        assert_eq!(ui.e.ok(&["show", &id, "--with", "all"]), before, "{mode}");
        assert_eq!(ui.text("messages"), "", "{mode}");
        assert_eq!(ui.text("renames"), "", "{mode}");
    }
}

#[test]
fn resume_never_falls_back_to_an_older_live_waiting_session() {
    let ui = WaitingUi::new();
    let id = ui.waiting("exit 75", true);
    ui.e.ok(&["reopen", &id]);
    ui.claim(&id, "new-session", false);
    leave_waiting(&ui, &id, None);
    let before = ui.e.ok(&["show", &id, "--with", "all"]);
    let error = failure(
        ui.cmd(),
        &["resume", &id, "-m", "Continue"],
        4,
        "invalid_state",
    );
    recovery_hint(&error, &id);
    assert_eq!(ui.e.ok(&["show", &id, "--with", "all"]), before);
    assert_eq!(ui.text("messages"), "");
}

#[test]
fn resume_serializes_with_live_checks_and_recovers_a_stale_runner() {
    let ui = WaitingUi::new();
    let id = ui.waiting(STUBBORN_CHECK, true);
    let before = ui.e.ok(&["show", &id, "--with", "all"]);
    let mut runner = CheckRunner::spawn(&ui.e);
    let pgid = runner.ready();
    check_wait_until("the check descendant", || {
        ui.e.repo().join("check-child").exists()
    });
    for stale_runner in [false, true] {
        if stale_runner {
            runner.signal("-KILL");
            assert!(!runner.wait().success());
            assert!(check_group_alive(pgid));
        }
        failure(
            ui.cmd(),
            &["resume", &id, "-m", "Not while checking"],
            4,
            "invalid_state",
        );
        assert_eq!(ui.e.ok(&["show", &id, "--with", "all"]), before);
        assert_eq!(ui.text("messages"), "");
        assert_check_busy(&ui.e, runner.child.id(), pgid);
    }
    std::fs::write(ui.e.repo().join("check-child-release"), "").unwrap();
    check_wait_until("the stale check group to finish", || {
        !check_group_alive(pgid)
    });
    success(ui.cmd(), &["resume", &id, "-m", "The check is now gone"]);
    assert_eq!(ui.e.ok(&["show", &id])["state"], "claimed");
    assert_eq!(ui.e.ok(&["whoami"])["check_running"], Value::Null);
    let messages = ui.text("messages");
    assert!(messages.starts_with("exact-origin\n"), "{messages}");
    assert!(
        messages.ends_with("\n\nThe check is now gone\n"),
        "{messages}"
    );
}

#[test]
fn resume_send_failure_releases_the_restored_claim_and_records_the_failure() {
    for send in [
        "send       = [\"missing-resume-send-program\", \"{name}\", \"{text}\"]",
        "send       = [\"sh\", \"-c\", \"echo resume-send-refused >&2; exit 9\"]",
    ] {
        let ui = WaitingUi::new();
        let id = ui.waiting("exit 75", true);
        let before = ui.e.ok(&["show", &id, "--with", "claims"]);
        ui.e.set_config(
            "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
            send,
        );
        failure(
            ui.cmd(),
            &["resume", &id, "-m", "Continue after approval"],
            1,
            "internal",
        );
        let issue = ui.e.ok(&["show", &id, "--with", "all"]);
        assert_eq!(issue["state"], "open");
        assert_eq!(issue["claim"], Value::Null);
        assert_eq!(issue["waiting"], Value::Null);
        assert_eq!(issue["derived"]["ready"], true);
        let claims = issue["claims"].as_array().unwrap();
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[0], before["claims"][0]);
        assert_eq!(claims[1]["actor"], before["claims"][0]["actor"]);
        assert_eq!(claims[1]["terminal"], "exact-origin");
        assert!(claims[1]["released_at"].is_string());
        assert!(claims[1]["release_reason"].is_string());
        assert!(
            issue["notes"].as_array().unwrap().iter().any(|note| {
                let body = note["body"].as_str().unwrap().to_lowercase();
                body.contains("resume") && (body.contains("send") || body.contains("deliver"))
            }),
            "{issue:#}"
        );
        assert!(ui.e.ok(&["runs"]).as_array().unwrap().is_empty());
        assert_eq!(ui.e.ok(&["whoami"])["check_running"], Value::Null);
        assert_eq!(ui.text("messages"), "");
    }
}

#[test]
fn plain_send_delivers_without_restoring_waiting_authority() {
    let ui = WaitingUi::new();
    let id = ui.waiting("exit 75", true);
    let before = ui.e.ok(&["show", &id, "--with", "all"]);
    let result = success(ui.cmd(), &["send", &id, "This is only a message"]);
    assert_eq!(result["terminal"], "exact-origin");
    assert_eq!(
        ui.text("messages"),
        "exact-origin\nThis is only a message\n"
    );
    assert_eq!(ui.e.ok(&["show", &id, "--with", "all"]), before);
}
