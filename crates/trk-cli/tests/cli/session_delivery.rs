use super::*;

#[test]
fn send_uses_configured_attempts_with_zero_and_normal_defaults() {
    for (retries, expected) in [(Some(0), 1), (Some(3), 3), (None, 10)] {
        let ui = WaitingUi::new();
        ui.e.set_config(
            r#"send       = ["hc", "send", "{name}", "{text}"]"#,
            r#"send = ["sh", "-c", "echo attempt >> \"$TRK_TEST_UI/attempts\"; echo refused >&2; exit 9"]"#,
        );
        ui.e.set_config(
            "send_retries = 10",
            &retries
                .map(|n| format!("send_retries = {n}"))
                .unwrap_or_default(),
        );
        ui.e.set_config(
            "send_retry_delay = 3",
            if retries.is_some() {
                "send_retry_delay = 0"
            } else {
                ""
            },
        );
        let id = ui.waiting("exit 75", false);
        let out = ui
            .cmd()
            .args(["--json", "send", &id, "hello"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        let response: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(response["error"]["code"], "internal");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("refused")
        );
        assert_eq!(ui.text("attempts").lines().count(), expected, "{retries:?}");
    }
}

#[test]
fn briefing_resume_and_check_delivery_use_configured_attempts() {
    for delivery in ["briefing", "resume", "wake", "notification"] {
        let ui = WaitingUi::new();
        ui.e.set_config(
            r#"send       = ["hc", "send", "{name}", "{text}"]"#,
            r#"send = ["sh", "-c", "echo attempt >> \"$TRK_TEST_UI/attempts\"; exit 9"]"#,
        );
        ui.e.set_config("send_retries = 10", "send_retries = 2");
        ui.e.set_config("send_retry_delay = 3", "send_retry_delay = 0");
        if delivery == "briefing" {
            ui.e.set_config("driver = \"herdr\"", "");
            ui.e.set_config(
                r#"start      = ["hc", "run", "--bg", "--no-parent", "--name", "{name}", "--cwd", "{cwd}", "--", "sh", "-c", "{shell_command}"]"#,
                r#"start = ["sh", "-c", "exit 0"]"#,
            );
            ui.e.set_config("send_delay = 5", "send_delay = 0");
            ui.e.set_config("[harness.omp]", "[harness.omp]\nbriefing = \"send\"");
            ui.e.set_config("command = \"omp\"", "command = \"sh\"");
            let id = ui.e.create(&["Briefing delivery"]);
            let result = resume::success(ui.cmd(), &["run", &id, "--harness", "omp"]);
            assert_eq!(result["briefing_sent"], false);
        } else {
            let check = if delivery == "notification" {
                "exit 0"
            } else {
                "exit 1"
            };
            let id = ui.waiting(check, false);
            if delivery == "resume" {
                let out = ui
                    .cmd()
                    .args(["--json", "resume", &id, "-m", "continue"])
                    .output()
                    .unwrap();
                assert_eq!(out.status.code(), Some(1));
                assert_eq!(ui.e.ok(&["show", &id])["state"], "open");
            } else {
                let result = ui.check(&id, "");
                assert_eq!(
                    result["state"],
                    if delivery == "wake" { "open" } else { "closed" }
                );
            }
        }
        assert_eq!(ui.text("attempts").lines().count(), 2, "{delivery}");
    }
}

#[test]
fn waiting_sessions_accept_send_and_attach_and_runs_labels_both_states() {
    let ui = WaitingUi::new();
    let waiting = ui.waiting("exit 75", false);
    let active = ui.e.create(&["Still working"]);
    ui.claim(&active, "new-session", false);

    for args in [
        vec!["send", waiting.as_str(), "keep going"],
        vec!["attach", waiting.as_str()],
    ] {
        let out = ui.cmd().arg("--json").args(&args).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(out.status.success(), "{args:?}: {v:#}");
        assert_eq!(v["data"]["terminal"], "exact-origin");
    }
    assert_eq!(
        ui.text("messages"),
        "exact-origin\n[trk-delivery source=trk-send actor=\"human:alice@example.com\"]\nkeep going\n[/trk-delivery]\n"
    );
    assert_eq!(ui.text("attachments"), "exact-origin\n");

    let runs = resume::success(ui.cmd(), &["runs"]);
    let rows = runs.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (id, state) in [(&waiting, "waiting"), (&active, "claimed")] {
        let row = rows.iter().find(|r| r["issue"] == *id).unwrap();
        assert_eq!(row["state"], state);
        assert_eq!(row["session_alive"], true);
        assert_eq!(row["mcp_alive"], false);
    }
    let out = ui
        .cmd()
        .args(["--format", "pretty", "runs"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let table = String::from_utf8(out.stdout).unwrap();
    assert!(table.lines().next().unwrap().contains("STATE"), "{table}");
    for (id, state) in [(&waiting, "waiting"), (&active, "claimed")] {
        let row = table.lines().find(|line| line.contains(id)).unwrap();
        assert!(row.contains(state), "{table}");
        assert!(row.contains("live") && row.contains("dormant"), "{table}");
    }
}

#[test]
fn send_and_attach_refuse_waiting_terminals_after_close_or_reopen() {
    for transition in ["close", "reopen"] {
        let ui = WaitingUi::new();
        let id = ui.waiting("exit 75", false);
        ui.e.ok(&[transition, &id]);

        for args in [
            vec!["send", id.as_str(), "do not deliver"],
            vec!["attach", id.as_str()],
        ] {
            let out = ui.cmd().arg("--json").args(&args).output().unwrap();
            let v: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(out.status.code(), Some(3), "{args:?}: {v:#}");
            assert_eq!(v["error"]["code"], "not_found");
        }
        assert_eq!(ui.text("messages"), "");
        assert_eq!(ui.text("attachments"), "");
        assert!(ui.e.ok(&["runs"]).as_array().unwrap().is_empty());
    }
}

#[test]
fn send_and_attach_prefer_active_claim_even_without_a_terminal() {
    for terminal in [Some("new-session"), None] {
        let ui = WaitingUi::new();
        let id = ui.waiting("exit 75", false);
        ui.e.ok(&["reopen", &id]);
        if let Some(terminal) = terminal {
            ui.claim(&id, terminal, true);
        } else {
            ui.e.ok(&["claim", &id, "--as", "agent:omp:replacement"]);
        }

        for args in [
            vec!["send", id.as_str(), "replacement only"],
            vec!["attach", id.as_str()],
        ] {
            let out = ui.cmd().arg("--json").args(&args).output().unwrap();
            let v: Value = serde_json::from_slice(&out.stdout).unwrap();
            if let Some(terminal) = terminal {
                assert!(out.status.success(), "{args:?}: {v:#}");
                assert_eq!(v["data"]["terminal"], terminal);
            } else {
                assert_eq!(out.status.code(), Some(3), "{args:?}: {v:#}");
                assert_eq!(v["error"]["code"], "not_found");
            }
        }
        if terminal.is_some() {
            assert_eq!(
                ui.text("messages"),
                "new-session\n[trk-delivery source=trk-send actor=\"human:alice@example.com\"]\nreplacement only\n[/trk-delivery]\n"
            );
            assert_eq!(ui.text("attachments"), "new-session\n");
        } else {
            assert_eq!(ui.text("messages"), "");
            assert_eq!(ui.text("attachments"), "");
        }
    }
}

#[test]
fn reap_skips_waiting_sessions_even_when_their_mcp_server_has_exited() {
    let ui = WaitingUi::new();
    let waiting = ui.waiting("exit 75", false);
    let dead = ui.e.create(&["Ended active session"]);
    ui.claim(&dead, "dead-session", false);
    let alive = ui.e.create(&["Live active session"]);
    ui.claim(&alive, "new-session", false);
    let before = ui.e.ok(&["show", &waiting, "--with", "all"]);

    let reaped = resume::success(ui.cmd(), &["reap"]);
    assert_eq!(reaped.as_array().unwrap().len(), 1);
    assert_eq!(reaped[0]["issue"], dead);
    assert_eq!(ui.e.ok(&["show", &dead])["state"], "open");
    assert_eq!(ui.e.ok(&["show", &alive])["state"], "claimed");
    assert_eq!(ui.e.ok(&["show", &waiting, "--with", "all"]), before);
    assert!(
        resume::success(ui.cmd(), &["reap"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    let runs = ui.e.ok(&["runs"]);
    assert_eq!(runs.as_array().unwrap().len(), 2);
    assert!(
        runs.as_array()
            .unwrap()
            .iter()
            .any(|row| row["issue"] == waiting && row["state"] == "waiting")
    );
}

#[test]
fn terminal_liveness_controls_dormant_mcp_wakes_and_reap() {
    for (mode, live) in [
        ("herdr-live", Some(true)),
        ("herdr-gone", Some(false)),
        ("herdr-pid-gone", Some(true)),
        ("herdr-unavailable", None),
        ("template-live", Some(true)),
        ("template-gone", Some(false)),
        ("template-error", Some(false)),
        ("template-missing", None),
        ("template-empty", None),
        ("template-unavailable", None),
        ("unknown-host", None),
        ("remote-host", None),
        ("blank-terminal", None),
    ] {
        let ui = WaitingUi::new();
        if mode.starts_with("template-") {
            ui.e.set_config("driver = \"herdr\"", "");
            let template = match mode {
                "template-live" => {
                    r#"["sh", "-c", "case \"$1\" in exact-origin|new-session) exit 0;; *) exit 1;; esac", "probe", "{name}"]"#
                }
                "template-gone" => r#"["sh", "-c", "exit 1"]"#,
                "template-error" => r#"["sh", "-c", "exit 2"]"#,
                "template-empty" => "[]",
                "template-unavailable" => r#"["missing-terminal-probe"]"#,
                _ => "",
            };
            if !template.is_empty() {
                ui.e.set_config(
                    "[launcher.herdr]",
                    &format!("[launcher.herdr]\nexists = {template}"),
                );
            }
        }
        let waiting = ui.waiting("echo dormant failure evidence; exit 1", false);
        let active = ui.e.create(&["Dormant working session"]);
        ui.claim(&active, "new-session", false);
        if mode == "herdr-pid-gone" {
            let store = trk_core::Store::open(
                &ui.e.trk_dir().join("trk.db"),
                trk_core::StoreOpts::default(),
            )
            .unwrap();
            store
                .conn()
                .execute(
                    "UPDATE actor SET meta = ?1 WHERE id LIKE 'agent:%'",
                    [json!({"mcp_pid": i64::MAX, "mcp_host": local_host()}).to_string()],
                )
                .unwrap();
        }
        let mut dashboard = ui.cmd();
        dashboard.env(
            "TRK_TEST_UI_FAILURE",
            match mode {
                "herdr-gone" => "missing",
                "herdr-unavailable" => "unavailable",
                _ => "",
            },
        );
        if matches!(mode, "unknown-host" | "remote-host" | "blank-terminal") {
            let store = trk_core::Store::open(
                &ui.e.trk_dir().join("trk.db"),
                trk_core::StoreOpts::default(),
            )
            .unwrap();
            if mode == "blank-terminal" {
                store
                    .conn()
                    .execute("UPDATE claim SET terminal = ' '", [])
                    .unwrap();
            } else {
                store
                    .conn()
                    .execute(
                        "UPDATE claim SET host = ?1",
                        [(mode == "remote-host").then_some("remote.invalid")],
                    )
                    .unwrap();
            }
        }
        let snapshot = resume::success(dashboard, &["dashboard"]);
        let session = snapshot["issues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|issue| issue["id"] == active)
            .unwrap();
        assert_eq!(
            session["session"]["alive"],
            json!(live),
            "{mode}: {session}"
        );
        assert_eq!(session["session"]["mcp_alive"], false, "{mode}: {session}");
        assert_eq!(
            session["group"],
            if live == Some(false) {
                "attention"
            } else {
                "running"
            },
            "{mode}: {session}"
        );
        let failure = match mode {
            "herdr-gone" => "missing",
            "herdr-unavailable" => "unavailable",
            _ => "",
        };
        let result = ui.check(&waiting, failure);
        assert_eq!(
            result["result"],
            if live == Some(true) {
                "woken"
            } else {
                "failed"
            },
            "{mode}: {result}"
        );
        let issue = ui.e.ok(&["show", &waiting, "--with", "notes,claims"]);
        if live == Some(true) {
            assert_eq!(issue["state"], "claimed", "{mode}: {issue}");
            let claims = issue["claims"].as_array().unwrap();
            assert_eq!(claims[0]["actor"], claims[1]["actor"]);
            assert!(
                ui.text("messages").contains("dormant failure evidence"),
                "{mode}"
            );
        } else {
            assert_eq!(issue["state"], "open", "{mode}: {issue}");
            assert_eq!(issue["claim"], Value::Null);
            assert!(
                issue["notes"].as_array().unwrap().iter().any(|note| {
                    note["body"]
                        .as_str()
                        .unwrap()
                        .contains("dormant failure evidence")
                }),
                "{mode}: {issue}"
            );
            assert_eq!(ui.text("messages"), "", "{mode}");
        }
        let mut reap = ui.cmd();
        reap.env("TRK_TEST_UI_FAILURE", failure);
        let reaped = resume::success(reap, &["reap"]);
        if live == Some(false) {
            assert_eq!(reaped[0]["issue"], active, "{mode}: {reaped}");
            assert_eq!(ui.e.ok(&["show", &active])["state"], "open");
        } else {
            assert_eq!(reaped, json!([]), "{mode}: {reaped}");
            assert_eq!(ui.e.ok(&["show", &active])["state"], "claimed");
        }
    }
}

