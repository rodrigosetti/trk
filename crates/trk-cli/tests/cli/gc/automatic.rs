use super::*;

#[test]
fn check_gc_missing_keys_default_to_enabled_with_one_hour_grace() {
    let fixture = GcEnv::new();
    check_gc_config(&fixture, "");
    let (id, path) = fixture.closed("done", "term-target");
    let notes = fixture.notes(&id);

    let before = check_gc_at(&fixture, "2026-11-20T10:59:59Z", &["check"]);
    assert!(before.get("gc").is_none(), "{before:#}");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);

    let after = check_gc_at(&fixture, "2026-11-20T11:00:00Z", &["check"]);
    assert_eq!(after["gc"]["dry_run"], false);
    for resource in ["session", "worktree", "branch"] {
        gc_status(gc_issue(&after["gc"], &id), resource, "removed");
    }
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert_eq!(fixture.closed_spaces(), ["target"]);
}

#[test]
fn check_gc_zero_grace_collects_issues_closed_in_the_same_pass() {
    let fixture = GcEnv::new();
    check_gc_config(&fixture, "gc_grace = \"0\"");
    let id = fixture.e.create(&["Close and collect"]);
    let path = fixture.worktree(&id);
    fixture.claim(&id, Some(&path), "term-target", &fixture.host);
    fixture
        .e
        .ok(&["wait", &id, "--check", "exit 0", "--reason", "testing"]);

    let data = check_gc_at(&fixture, "2026-11-20T10:00:00Z", &["check"]);
    assert_eq!(data["results"][0]["issue"], id);
    assert_eq!(data["results"][0]["result"], "passed");
    gc_status(gc_issue(&data["gc"], &id), "worktree", "removed");
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert_eq!(fixture.e.ok(&["show", &id])["state"], "closed");
}

#[test]
fn check_gc_can_be_disabled_without_disabling_manual_gc() {
    for settings in ["gc = false", "gc = true"] {
        let fixture = GcEnv::new();
        check_gc_config(&fixture, settings);
        let (id, path) = fixture.closed("done", "term-target");
        let notes = fixture.notes(&id);
        let args: &[&str] = if settings == "gc = false" {
            &["check"]
        } else {
            &["check", "--no-gc"]
        };
        let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", args);
        assert!(data.get("gc").is_none(), "{data:#}");
        assert!(path.exists());
        assert!(fixture.branch_exists(&id));
        assert!(fixture.closed_spaces().is_empty());
        assert_eq!(fixture.notes(&id), notes);

        gc_status(
            gc_issue(&fixture.run(&["gc", &id]), &id),
            "worktree",
            "removed",
        );
        assert!(!path.exists());
    }
}

#[test]
fn check_gc_explicit_id_does_not_sweep_other_closed_issues() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    let waiting = fixture.e.waiting("Selected check", "exit 0");
    let notes = fixture.notes(&id);
    let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check", &waiting]);
    assert_eq!(data["results"][0]["result"], "passed");
    assert!(data.get("gc").is_none(), "{data:#}");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn check_gc_ignores_abandoned_issues() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("abandoned", "term-target");
    let notes = fixture.notes(&id);
    let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    assert!(data.get("gc").is_none(), "{data:#}");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn check_gc_dry_run_previews_without_changing_resources_notes_or_database() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    let waiting = fixture.e.waiting("Preview check", "exit 0");
    let before = fixture.e.ok(&["show", &waiting, "--with", "notes"]);
    let notes = fixture.notes(&id);
    let database = fixture.e.trk_dir().join("trk.db");
    let bytes = std::fs::read(&database).unwrap();

    let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check", "--dry-run"]);
    assert_eq!(data["gc"]["dry_run"], true);
    let issue = gc_issue(&data["gc"], &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
        assert!(
            issue[resource]["reason"]
                .as_str()
                .unwrap()
                .starts_with("would remove ")
        );
    }
    assert_eq!(std::fs::read(database).unwrap(), bytes);
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
    assert_eq!(fixture.e.ok(&["show", &waiting, "--with", "notes"]), before);
}

#[test]
fn check_gc_session_failure_does_not_change_check_results_or_exit_status() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    std::fs::write(
        fixture.bin.join("herdr-app"),
        r#"#!/bin/sh
if [ "$1" = ls ]; then
    printf '%s\n' '{"spaces":[{"id":"target","tabs":[{"terminals":[{"terminal":"term-target"}]}]}]}'
else
    exit 1
fi
"#,
    )
    .unwrap();
    let waiting = fixture.e.waiting("Still closes", "exit 0");

    let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    assert_eq!(data["results"][0]["issue"], waiting);
    assert_eq!(data["results"][0]["result"], "passed");
    assert_eq!(fixture.e.ok(&["show", &waiting])["state"], "closed");
    assert!(
        data["gc"]["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty()),
        "{data:#}"
    );
    gc_status(gc_issue(&data["gc"], &id), "worktree", "removed");
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert!(!fixture.notes(&id).as_array().unwrap().is_empty());
}

#[test]
fn check_gc_watch_sweeps_each_pass_and_keeps_skipped_resources_quiet() {
    let fixture = GcEnv::new();
    check_gc_config(&fixture, "gc_grace = \"0\"");
    let (first, first_path) = fixture.closed("done", "term-target");
    let (skipped, skipped_path) = fixture.closed("done", "missing-terminal");
    std::fs::write(skipped_path.join("dirty"), "keep this work").unwrap();
    let skipped_notes = fixture.notes(&skipped);
    let primary = fixture.e.create(&["Historical primary-checkout cwd"]);
    fixture.claim(
        &primary,
        Some(&fixture.e.repo()),
        "missing-terminal",
        &fixture.host,
    );
    fixture.close_at(&primary, "done", "2026-11-20T10:00:00Z");
    fixture.git_at(&fixture.e.repo(), &["branch", &primary]);
    let absent = fixture.e.create(&["Absent outside-root worktree"]);
    let absent_path = fixture.e.dir.path().join("old-worktrees").join(&absent);
    fixture.claim(
        &absent,
        Some(&absent_path),
        "missing-terminal",
        &fixture.host,
    );
    fixture.close_at(&absent, "done", "2026-11-20T10:00:00Z");
    let mut unnamed = Vec::new();
    for (terminal, launcher) in [("term-active", None), ("term-decoy", Some(""))] {
        let id = fixture.e.create(&["Historical terminal without a profile"]);
        fixture.claim(&id, None, terminal, &fixture.host);
        fixture
            .store_at("2026-11-20T10:00:00Z")
            .conn()
            .execute(
                "UPDATE claim SET launcher = ?1 WHERE issue_id = ?2",
                (launcher, id.as_str()),
            )
            .unwrap();
        fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
        unnamed.push(id);
    }
    fixture.e.waiting(
        "Watch heartbeat",
        "echo pass >> \"$TRK_REPO/watch-passes\"; exit 75",
    );
    let output_path = fixture.e.dir.path().join("watch-output");
    let stderr_path = fixture.e.dir.path().join("watch-stderr");
    let path = std::env::join_paths(
        std::iter::once(fixture.bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let watch_command = || {
        let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
        command
            .args(["check", "--all", "--watch", "1s"])
            .env("PATH", &path)
            .env("TRK_TEST_UI", fixture.e.dir.path())
            .env_remove("HC_TERMINAL_ID")
            .stdout(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&output_path)
                    .unwrap(),
            )
            .stderr(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&stderr_path)
                    .unwrap(),
            );
        command
    };
    let mut runner = CheckRunner::spawn_command(&fixture.e, watch_command());
    check_wait_until("the first watch pass to collect a closed issue", || {
        !first_path.exists()
    });

    let second = fixture.e.create(&["Closed between watch passes"]);
    let second_path = fixture.worktree(&second);
    fixture.claim(&second, Some(&second_path), "term-history", &fixture.host);
    fixture.close_at(&second, "done", "2026-11-20T10:00:00Z");
    check_wait_until(
        "a later watch pass to collect a manually closed issue",
        || !second_path.exists(),
    );
    let passes = || {
        std::fs::read_to_string(fixture.e.repo().join("watch-passes"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let before = passes();
    check_wait_until("two more watch passes with only skipped resources", || {
        passes() >= before + 2
    });
    runner.signal("-INT");
    runner.wait();

    // Restarting must not reintroduce warnings for non-resources or absent paths.
    let before = passes();
    let mut runner = CheckRunner::spawn_command(&fixture.e, watch_command());
    check_wait_until("two restarted watch passes", || passes() >= before + 2);
    runner.signal("-INT");
    runner.wait();

    let output = std::fs::read_to_string(output_path).unwrap();
    assert_eq!(output.matches(&first).count(), 1, "{output}");
    assert_eq!(output.matches(&second).count(), 1, "{output}");
    assert_eq!(output.matches(&skipped).count(), 2, "{output}");
    assert!(!output.contains(&primary), "{output}");
    assert!(!output.contains(&absent), "{output}");
    assert!(fixture.branch_exists(&primary));
    assert!(fixture.e.repo().is_dir());
    for id in unnamed {
        assert_eq!(output.matches(&id).count(), 2, "{output}");
    }
    assert!(!output.contains("will retry"), "{output}");
    let stderr = std::fs::read_to_string(stderr_path).unwrap();
    assert!(!stderr.contains("claim.launcher"), "{stderr}");
    let mut closed = fixture.closed_spaces();
    closed.sort();
    assert_eq!(closed, ["history", "target"]);
    assert!(skipped_path.exists());
    assert!(fixture.branch_exists(&skipped));
    assert_eq!(fixture.notes(&skipped), skipped_notes);
}

#[test]
fn check_gc_collected_targets_skip_resource_probes_until_reopened_and_closed() {
    let fixture = GcEnv::new();
    check_gc_config(&fixture, "gc_grace = \"0\"");
    let (id, path) = fixture.closed("done", "term-target");
    fixture.log_git_calls();
    let first = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    for resource in ["session", "worktree", "branch"] {
        gc_status(gc_issue(&first["gc"], &id), resource, "removed");
    }
    assert!(!path.exists());
    assert!(!fixture.calls("herdr").is_empty());
    assert!(fixture.calls("git").contains(&id));
    let notes = fixture.notes(&id);
    fixture.clear_calls();
    let without_gc = check_gc_at(&fixture, "2026-11-20T12:00:01Z", &["check", "--no-gc"]);
    assert!(without_gc.get("gc").is_none(), "{without_gc:#}");
    // Environment discovery lists worktrees even without GC. A collected
    // sweep must add no probes beyond that same command's discovery calls.
    let discovery_git_calls = fixture.calls("git");
    assert_eq!(fixture.calls("herdr"), "");
    fixture.clear_calls();

    let second = check_gc_at(&fixture, "2026-11-20T12:00:01Z", &["check"]);
    assert!(second.get("gc").is_none(), "{second:#}");
    assert_eq!(fixture.calls("herdr"), "");
    let git_calls = fixture.calls("git");
    assert!(!git_calls.contains(&id), "{git_calls}");
    assert_eq!(git_calls, discovery_git_calls);
    assert_eq!(fixture.notes(&id), notes);

    fixture.e.ok(&["reopen", &id]);
    fixture.close_at(&id, "done", "2026-11-20T12:00:02Z");
    fixture.clear_calls();
    let reopened = check_gc_at(&fixture, "2026-11-20T12:00:03Z", &["check"]);
    for resource in ["session", "worktree", "branch"] {
        gc_status(gc_issue(&reopened["gc"], &id), resource, "absent");
    }
    assert!(!fixture.calls("herdr").is_empty());
    assert!(fixture.calls("git").contains(&id));
}

#[test]
fn check_gc_mixed_removed_and_skipped_items_remain_eligible_for_collection() {
    let fixture = GcEnv::new();
    check_gc_config(&fixture, "gc_grace = \"0\"");
    let id = fixture.e.create(&["Mixed historical sessions"]);
    fixture.claim(&id, None, "term-history", "foreign.invalid");
    fixture.e.ok(&["unclaim", &id]);
    let path = fixture.worktree(&id);
    fixture.claim(&id, Some(&path), "term-target", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");

    let first = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    let issue = gc_issue(&first["gc"], &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
    }
    let sessions = issue["session"]["items"].as_array().unwrap();
    assert!(sessions.iter().any(|item| item["status"] == "removed"));
    assert!(sessions.iter().any(|item| item["status"] == "skipped"));
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert_eq!(fixture.closed_spaces(), ["target"]);

    let second = check_gc_at(&fixture, "2026-11-20T12:00:01Z", &["check"]);
    gc_status(gc_issue(&second["gc"], &id), "session", "skipped");
    fixture
        .store_at("2026-11-20T12:00:02Z")
        .conn()
        .execute(
            "UPDATE claim SET host = ?1 WHERE issue_id = ?2 AND host = 'foreign.invalid'",
            (fixture.host.as_str(), id.as_str()),
        )
        .unwrap();
    let third = check_gc_at(&fixture, "2026-11-20T12:00:03Z", &["check"]);
    gc_status(gc_issue(&third["gc"], &id), "session", "removed");
    assert_eq!(fixture.closed_spaces(), ["target", "history"]);
    let fourth = check_gc_at(&fixture, "2026-11-20T12:00:04Z", &["check"]);
    assert!(fourth.get("gc").is_none(), "{fourth:#}");
}
