use super::*;

fn pass_wait(ui: &WaitingUi, check: &str, alive: bool) -> String {
    let id = ui.e.create(&["Post-pass work"]);
    ui.claim(&id, "exact-origin", alive);
    ui.e.ok(&[
        "wait",
        &id,
        "--force",
        "--check",
        check,
        "--on-pass",
        "wake",
        "--reason",
        "verify the installed binary after landing",
    ]);
    id
}

#[test]
fn passing_check_wakes_with_no_failure_budget_and_original_actor_can_close() {
    let ui = WaitingUi::new();
    ui.e.set_config("max_wakes = 5", "max_wakes = 0");
    let id = pass_wait(&ui, "exit 0", true);
    let original = ui.e.ok(&["show", &id, "--with", "claims"])["claims"][0].clone();
    let result = ui.check(&id, "");
    assert_eq!(result["result"], "pass_woken");
    assert_eq!(result["state"], "claimed");
    let issue = ui.e.ok(&["show", &id]);
    assert_eq!(issue["claim"]["actor"], original["actor"]);
    assert_eq!(issue["claim"]["terminal"], original["terminal"]);
    assert!(issue.get("wake_count").is_none());
    assert_eq!(issue["close_reason"], Value::Null);
    let messages = ui.text("messages");
    assert!(messages.starts_with("exact-origin\n"), "{messages}");
    assert!(messages.contains("check-pass-wake"), "{messages}");
    assert!(messages.contains("waiting check passed"), "{messages}");
    assert!(
        messages.contains("verify the installed binary after landing"),
        "{messages}"
    );
    assert_eq!(ui.text("space-name"), "Reviewed title");
    assert_eq!(ui.text("renames"), "");
    let mut command = ui.cmd();
    command.env("TRK_ACTOR", original["actor"].as_str().unwrap());
    assert_eq!(resume::success(command, &["close", &id])["state"], "closed");
}

#[test]
fn pass_woken_session_can_wait_for_another_check_without_a_wake_loop() {
    let ui = WaitingUi::new();
    let id = pass_wait(&ui, "exit 0", true);
    assert_eq!(ui.check(&id, "")["result"], "pass_woken");
    let actor = ui.e.ok(&["show", &id])["claim"]["actor"]
        .as_str()
        .unwrap()
        .to_owned();
    let messages = ui.text("messages");
    let mut command = ui.cmd();
    command.env("TRK_ACTOR", actor);
    resume::success(command, &["wait", &id, "--check", "exit 0"]);
    assert_eq!(ui.e.ok(&["show", &id])["waiting"]["on_pass"], "close");
    assert_eq!(ui.check(&id, "")["result"], "passed");
    assert_eq!(ui.e.ok(&["show", &id])["state"], "closed");
    let after = ui.text("messages");
    assert!(after.starts_with(&messages));
    assert_eq!(after.matches("check-pass-wake").count(), 1, "{after}");
    assert_eq!(ui.text("space-name"), "[closed] Reviewed title");
}

#[test]
fn passing_check_reopens_with_pending_work_when_wake_is_unavailable_or_fails() {
    for mode in [
        "missing",
        "unavailable",
        "no-send",
        "send-error",
        "send-rejected",
    ] {
        let ui = WaitingUi::new();
        match mode {
            "no-send" => ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "",
            ),
            "send-error" => ui.e.set_config(
                "send       = [\"hc\",",
                "send       = [\"missing-send-program\",",
            ),
            "send-rejected" => ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "send       = [\"sh\", \"-c\", \"exit 1\"]",
            ),
            _ => {}
        }
        let id = pass_wait(&ui, "exit 0", false);
        let result = ui.check(&id, mode);
        assert_eq!(result["result"], "pass_reopened", "{mode}: {result}");
        assert_eq!(result["state"], "open", "{mode}: {result}");
        let issue = ui.e.ok(&["show", &id, "--with", "notes,claims"]);
        assert_eq!(issue["claim"], Value::Null, "{mode}: {issue}");
        assert_eq!(issue["close_reason"], Value::Null, "{mode}: {issue}");
        assert!(
            issue["notes"].as_array().unwrap().iter().any(|note| {
                let body = note["body"].as_str().unwrap();
                body.contains("check passed")
                    && body.contains("verify the installed binary after landing")
            }),
            "{mode}: {issue}"
        );
        assert_eq!(ui.text("messages"), "", "{mode}");
        assert_eq!(ui.text("renames"), "", "{mode}");
    }
}

#[test]
fn waiting_check_marks_only_the_origin_space_once_and_preserves_notifications() {
    let ui = WaitingUi::new();
    let id = ui.waiting("exit 0", true);
    let replacement = ui.e.create(&["Replacement session"]);
    ui.claim(&replacement, "new-session", true);

    let result = ui.check(&id, "");
    assert_eq!(result["result"], "passed");
    assert_eq!(result["state"], "closed");
    assert_eq!(ui.e.ok(&["show", &id])["close_reason"], "done");
    assert_eq!(ui.text("space-name"), "[closed] Reviewed title");
    let messages = ui.text("messages");
    assert!(messages.starts_with("exact-origin\n"), "{messages}");
    assert!(messages.contains("waiting check passed"), "{messages}");
    assert_eq!(ui.e.ok(&["show", &replacement])["state"], "claimed");

    ui.e.ok(&["reopen", &id]);
    ui.claim(&id, "exact-origin", true);
    ui.e.ok(&["wait", &id, "--force", "-c", "exit 0"]);
    assert_eq!(ui.check(&id, "")["state"], "closed");
    assert_eq!(ui.text("space-name"), "[closed] Reviewed title");
    assert_eq!(ui.text("renames"), "renamed\n");
}

#[test]
fn waiting_check_normalizes_edge_markers_and_preserves_embedded_literal_text() {
    for (title, expected) in [
        (
            "Append [closed] to task titles",
            "[closed] Append [closed] to task titles",
        ),
        ("Reviewed title [closed]", "[closed] Reviewed title"),
        (
            "Reviewed title [closed] [closed]",
            "[closed] Reviewed title",
        ),
        ("[closed] Reviewed title", "[closed] Reviewed title"),
        (
            "[closed] Reviewed title [closed]",
            "[closed] Reviewed title",
        ),
        (
            "Append [closed] to titles [closed]",
            "[closed] Append [closed] to titles",
        ),
    ] {
        let ui = WaitingUi::new();
        std::fs::write(ui.e.dir.path().join("space-name"), title).unwrap();
        let id = ui.waiting("exit 0", false);
        assert_eq!(ui.check(&id, "")["state"], "closed");
        assert_eq!(ui.text("space-name"), expected);
        assert_eq!(
            ui.text("renames"),
            if title == expected { "" } else { "renamed\n" },
            "{title}"
        );
    }
}

#[test]
fn failed_and_retrying_waiting_checks_do_not_mark_spaces_closed() {
    for (exit, result, state) in [(1, "woken", "claimed"), (75, "retry", "waiting")] {
        let ui = WaitingUi::new();
        ui.e.set_config("max_wakes = 5", "");
        let id = ui.waiting(&format!("echo failure evidence; exit {exit}"), true);
        let out = ui.check(&id, "");
        assert_eq!(out["result"], result);
        assert_eq!(out["state"], state);
        assert_eq!(ui.text("space-name"), "Reviewed title");
        assert_eq!(ui.text("renames"), "");
        let messages = ui.text("messages");
        if exit == 1 {
            assert!(messages.starts_with("exact-origin\n"), "{messages}");
            assert!(messages.contains("waiting check failed"), "{messages}");
            assert!(messages.contains("wake 1 of 5"), "{messages}");
            assert!(messages.contains("failure evidence"), "{messages}");
            assert!(messages.contains("Address the failure"), "{messages}");
            assert!(messages.contains("calling wait"), "{messages}");
            let issue = ui.e.ok(&["show", &id, "--with", "events,claims,notes"]);
            assert_eq!(issue["claim"]["terminal"], "exact-origin");
            let claims = issue["claims"].as_array().unwrap();
            assert_eq!(claims[0]["actor"], claims[1]["actor"]);
            let events = issue["events"].as_array().unwrap();
            assert!(
                events
                    .iter()
                    .any(|e| e["op"] == "check" && e["payload"]["result"] == "failed")
            );
            assert_eq!(events.last().unwrap()["op"], "claim");
        } else {
            assert_eq!(messages, "");
        }
    }
}

#[test]
fn wake_budget_is_shown_in_both_active_states_and_caps_delivered_wakes() {
    let ui = WaitingUi::new();
    ui.e.set_config("max_wakes = 5", "max_wakes = 2");
    let check = "echo capped failure evidence; exit 1";
    let id = ui.waiting(check, true);
    assert!(ui.e.ok(&["show", &id]).get("wake_count").is_none());
    for count in 1..=2 {
        assert_eq!(ui.check(&id, "")["result"], "woken");
        let issue = ui.e.ok(&["show", &id]);
        assert_eq!(issue["state"], "claimed");
        assert_eq!(issue["wake_count"], count);
        let messages = ui.text("messages");
        assert!(
            messages.contains(&format!("wake {count} of 2")),
            "{messages}"
        );
        for state in ["claimed", "waiting"] {
            if state == "waiting" {
                ui.e.ok(&["wait", &id, "--force", "-c", check]);
            }
            let issue = ui.e.ok(&["show", &id]);
            assert_eq!(issue["state"], state);
            assert_eq!(issue["wake_count"], count);
            let out =
                ui.e.cmd()
                    .args(["--format", "pretty", "show", &id])
                    .output()
                    .unwrap();
            assert!(out.status.success());
            let pretty = String::from_utf8(out.stdout).unwrap();
            assert!(
                pretty
                    .lines()
                    .any(|line| { line.contains("wakes") && line.contains(&count.to_string()) }),
                "{pretty}"
            );
        }
    }
    let messages = ui.text("messages");
    let result = ui.check(&id, "");
    assert_eq!(result["result"], "failed");
    assert_eq!(result["state"], "open");
    assert_eq!(ui.text("messages"), messages);
    let issue = ui.e.ok(&["show", &id, "--with", "notes"]);
    assert!(issue.get("wake_count").is_none());
    assert!(
        issue["notes"].as_array().unwrap().iter().any(|note| {
            let body = note["body"].as_str().unwrap();
            body.contains("wake cap of 2 reached") && body.contains("capped failure evidence")
        }),
        "{issue}"
    );
}

#[test]
fn zero_wake_budget_never_delivers_to_a_live_waiting_session() {
    let ui = WaitingUi::new();
    ui.e.set_config("max_wakes = 5", "max_wakes = 0");
    let id = ui.waiting("echo disabled wake evidence; exit 1", true);
    let result = ui.check(&id, "");
    assert_eq!(result["result"], "failed");
    assert_eq!(result["state"], "open");
    assert_eq!(ui.text("messages"), "");
    let issue = ui.e.ok(&["show", &id, "--with", "claims"]);
    assert!(issue.get("wake_count").is_none());
    assert_eq!(issue["claims"].as_array().unwrap().len(), 1);
}

#[test]
fn failed_waiting_checks_reopen_when_session_is_gone_or_wake_cannot_be_sent() {
    for mode in [
        "no-send",
        "missing",
        "unavailable",
        "send-error",
        "send-rejected",
    ] {
        let ui = WaitingUi::new();
        match mode {
            "no-send" => ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "",
            ),
            "send-error" => ui.e.set_config(
                "send       = [\"hc\",",
                "send       = [\"missing-send-program\",",
            ),
            "send-rejected" => ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "send       = [\"sh\", \"-c\", \"exit 1\"]",
            ),
            _ => {}
        }
        let id = ui.waiting("echo failure evidence; exit 2", false);
        let result = ui.check(&id, mode);
        assert_eq!(result["result"], "failed", "{mode}: {result}");
        assert_eq!(result["state"], "open", "{mode}: {result}");
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("failure evidence")
        );
        let issue = ui.e.ok(&["show", &id, "--with", "events,claims,notes"]);
        assert_eq!(issue["claim"], Value::Null, "{mode}: {issue}");
        assert_eq!(issue["derived"]["ready"], true, "{mode}: {issue}");
        assert!(
            issue["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| { n["body"].as_str().unwrap().contains("failure evidence") })
        );
        let claims = issue["claims"].as_array().unwrap();
        if mode.starts_with("send-") {
            assert_eq!(claims.len(), 2, "{mode}: {claims:?}");
            assert!(claims[1]["released_at"].is_string(), "{mode}: {claims:?}");
        } else {
            assert_eq!(claims.len(), 1, "{mode}: {claims:?}");
        }
        assert_eq!(ui.text("messages"), "", "{mode}");
        assert_eq!(ui.text("renames"), "", "{mode}");
    }
}

#[test]
fn timed_out_waiting_check_wakes_its_live_session() {
    let ui = WaitingUi::new();
    ui.e.set_config("timeout  = \"10m\"", "timeout  = \"1s\"");
    let id = ui.waiting("echo timeout evidence; sleep 30", true);
    let result = ui.check(&id, "");
    assert_eq!(result["result"], "woken");
    assert_eq!(result["state"], "claimed");
    assert_eq!(result["timed_out"], true);
    let message = ui.text("messages");
    assert!(message.contains("timed out"), "{message}");
    assert!(message.contains("timeout evidence"), "{message}");
}

#[test]
fn waiting_check_ui_and_message_failures_are_independent_and_nonfatal() {
    for mode in [
        "dormant",
        "no-send",
        "send",
        "rename",
        "unavailable",
        "missing",
        "headless",
    ] {
        let ui = WaitingUi::new();
        match mode {
            "no-send" => ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "",
            ),
            "send" => ui.e.set_config(
                "send       = [\"hc\",",
                "send       = [\"missing-send-program\",",
            ),
            "headless" => ui.e.set_config("driver = \"herdr\"", ""),
            _ => {}
        }
        let id = ui.waiting("exit 0", mode != "dormant");
        assert_eq!(ui.check(&id, mode)["state"], "closed", "{mode}");
        assert_eq!(ui.e.ok(&["show", &id])["state"], "closed", "{mode}");
        let renamed = matches!(mode, "dormant" | "no-send" | "send");
        assert_eq!(
            ui.text("space-name"),
            if renamed {
                "[closed] Reviewed title"
            } else {
                "Reviewed title"
            },
            "{mode}"
        );
        let notified = matches!(mode, "dormant" | "rename");
        assert_eq!(!ui.text("messages").is_empty(), notified, "{mode}");
    }
}
