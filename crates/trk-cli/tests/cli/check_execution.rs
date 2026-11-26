use super::*;

#[test]
fn checks_pass_retry_and_fail() {
    let e = Env::new();
    // Not yet: exit 75 keeps it waiting, with no event, and it is not due again
    // until the interval has passed.
    let slow = e.waiting("slow", "echo still open; exit 75");
    let before = e.ok(&["log", &slow]).as_array().unwrap().len();
    let r = e.ok(&["check"]);
    assert_eq!(r["results"][0]["result"], "retry");
    assert_eq!(r["results"][0]["exit"], 75);
    let v = e.ok(&["show", &slow]);
    assert_eq!(v["state"], "waiting");
    assert_eq!(v["waiting"]["last_check_exit"], 75);
    assert_eq!(v["waiting"]["next_check_at"], "2026-11-20T10:05:00Z");
    assert_eq!(e.ok(&["log", &slow]).as_array().unwrap().len(), before);
    assert!(e.ok(&["check"])["results"].as_array().unwrap().is_empty());
    assert_eq!(e.ok(&["check", "--all"])["results"][0]["result"], "retry");
    assert_eq!(
        e.at("2026-11-20T10:06:00Z", &["check"])["results"][0]["issue"],
        json!(slow)
    );

    // The dry run shows the command and where it would run.
    let d = e.ok(&["check", "--all", "--dry-run"]);
    assert_eq!(d["results"][0]["result"], "would_run");
    assert_eq!(d["results"][0]["check"], "echo still open; exit 75");
    assert!(d["results"][0]["cwd"].as_str().unwrap().ends_with("/repo"));

    // A failure returns the issue to open with the output and the
    // environment the check saw.
    let bad = e.waiting(
        "bad",
        "echo boom; echo \"$TRK_ISSUE $TRK_BRANCH $(basename \"$TRK_REPO\") $(pwd | xargs basename)\"; exit 3",
    );
    let r = e.ok(&["check", &bad]);
    assert_eq!(r["results"][0]["result"], "failed");
    assert_eq!(r["results"][0]["exit"], 3);
    assert!(r["results"][0]["output"].as_str().unwrap().contains("boom"));
    let v = e.ok(&["show", &bad]);
    assert_eq!(v["state"], "open");
    assert_eq!(v["derived"]["ready"], true);
    let note = v["notes"].as_array().unwrap().last().unwrap()["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(note.contains("exited with 3"), "{note}");
    assert!(note.contains(&format!("{bad} main repo repo")), "{note}");
    // There is nothing left to check on it.
    e.err(&["check", &bad], 4, "invalid_state");

    // A command that cannot run is a failure, not a wait.
    let missing = e.waiting("missing", "definitely-not-a-command-xyz");
    assert_eq!(e.ok(&["check", &missing])["results"][0]["exit"], 127);
    assert_eq!(e.ok(&["show", &missing])["state"], "open");

    // A worktree that has gone cannot be checked in.
    let gone = e.create(&["gone"]);
    let wt = e.dir.path().join("vanished");
    std::fs::create_dir_all(&wt).unwrap();
    e.ok(&["claim", &gone, "--worktree", wt.to_str().unwrap()]);
    e.ok(&["wait", &gone, "-c", "true"]);
    std::fs::remove_dir_all(&wt).unwrap();
    let r = e.ok(&["check", &gone]);
    assert_eq!(r["results"][0]["result"], "failed");
    assert!(
        r["results"][0]["output"]
            .as_str()
            .unwrap()
            .contains("no longer exists")
    );
}

#[test]
fn waiting_on_a_human_has_no_check() {
    let e = Env::new();
    let id = e.create(&["Pick a database"]);
    e.err(&["wait", &id, "-r", "which one?"], 4, "invalid_state");
    e.ok(&["claim", &id]);
    e.err(&["wait", &id], 4, "invalid_field");
    let v = e.ok(&["wait", &id, "-r", "SQLite or Dolt?"]);
    assert_eq!(v["waiting"]["check"], Value::Null);
    assert_eq!(v["derived"]["due"], false);
    assert!(
        e.ok(&["check", "--all"])["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    e.err(&["check", &id], 4, "invalid_state");
    e.err(&["claim", &id], 4, "invalid_state");
    // A human replaces the wait with a check, or answers and reopens.
    let v = e.ok(&["wait", &id, "-c", "exit 75", "-r", "now automated"]);
    assert_eq!(v["waiting"]["check"], "exit 75");
    assert_eq!(v["waiting"]["interval"], 300);
    assert_eq!(e.ok(&["reopen", &id, "-m", "SQLite"])["state"], "open");
}

#[test]
fn a_check_that_runs_too_long_is_killed() {
    let e = Env::new();
    e.set_config("timeout  = \"10m\"", "timeout  = \"1s\"");
    let id = e.waiting("hangs", "echo started; sleep 30; exit 0");
    let t = std::time::Instant::now();
    let r = e.ok(&["check"]);
    assert!(t.elapsed().as_secs() < 15, "the check was not killed");
    assert_eq!(r["results"][0]["result"], "failed");
    assert_eq!(r["results"][0]["timed_out"], true);
    let v = e.ok(&["show", &id]);
    assert_eq!(v["state"], "open");
    let note = v["notes"].as_array().unwrap().last().unwrap()["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(note.contains("timed out"), "{note}");
    assert!(note.contains("started"), "{note}");
}

#[test]
fn only_one_check_runs_at_a_time() {
    let e = Env::new();
    let id = e.waiting(
        "slow merge",
        "echo $$ > \"$TRK_REPO/check-shell\"; \
         while [ ! -f \"$TRK_REPO/check-release\" ]; do sleep 0.02; done; exit 75",
    );
    let mut first = CheckRunner::spawn(&e);
    let pgid = first.ready();

    // A broken lock must fail through a side effect, not hang a second runner
    // on the first command's readiness gate.
    e.ok(&[
        "wait",
        &id,
        "-c",
        "echo second > \"$TRK_REPO/check-second\"; exit 75",
    ]);
    assert_check_busy(&e, first.child.id(), pgid);
    assert!(!e.repo().join("check-second").exists());
    std::fs::write(e.repo().join("check-release"), "").unwrap();
    assert!(first.wait().success());
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
    assert_eq!(e.ok(&["check", "--all"])["results"][0]["result"], "retry");
    assert!(e.repo().join("check-second").exists());
}

fn check_interruption(signal: &str) {
    let e = Env::new();
    let id = e.waiting("interrupted check", STUBBORN_CHECK);
    let before = e.ok(&["show", &id]);
    let log_before = e.ok(&["log", &id]);
    let mut runner = CheckRunner::spawn(&e);
    let pgid = runner.ready();
    check_wait_until("the TERM-resistant descendant to start", || {
        std::fs::read_to_string(e.repo().join("check-child"))
            .is_ok_and(|s| s.trim().parse::<i64>().is_ok())
    });
    runner.signal(signal);
    runner.wait();
    assert!(
        !check_group_alive(pgid),
        "a check descendant survived {signal}"
    );
    let after = e.ok(&["show", &id]);
    assert_eq!(after["state"], "waiting");
    assert_eq!(after["waiting"], before["waiting"]);
    assert_eq!(after["notes"], before["notes"]);
    assert_eq!(e.ok(&["log", &id]), log_before);
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
    assert!(
        !e.trk_dir().join("checks").exists()
            || std::fs::read_dir(e.trk_dir().join("checks"))
                .unwrap()
                .next()
                .is_none(),
        "interrupted check left a log behind"
    );

    e.ok(&[
        "wait",
        &id,
        "-c",
        "echo second > \"$TRK_REPO/check-second\"; exit 75",
    ]);
    assert_eq!(e.ok(&["check", "--all"])["results"][0]["result"], "retry");
    assert!(e.repo().join("check-second").exists());
}

#[test]
fn check_sigint_cleans_up_without_recording_a_result() {
    check_interruption("-INT");
}

#[test]
fn check_sigterm_cleans_up_without_recording_a_result() {
    check_interruption("-TERM");
}

#[test]
fn check_sighup_cleans_up_without_recording_a_result() {
    check_interruption("-HUP");
}

#[test]
fn check_sigkill_keeps_ownership_until_the_old_group_is_gone() {
    let e = Env::new();
    let id = e.waiting("crashed check", STUBBORN_CHECK);
    let before = e.ok(&["show", &id]);
    let mut runner = CheckRunner::spawn(&e);
    let pgid = runner.ready();
    check_wait_until("the crash survivor to start", || {
        std::fs::read_to_string(e.repo().join("check-child"))
            .is_ok_and(|s| s.trim().parse::<i64>().is_ok())
    });
    runner.signal("-KILL");
    assert!(!runner.wait().success());
    assert!(check_group_alive(pgid));
    let after = e.ok(&["show", &id]);
    assert_eq!(after["state"], "waiting");
    assert_eq!(after["waiting"], before["waiting"]);

    e.ok(&[
        "wait",
        &id,
        "-c",
        "echo second > \"$TRK_REPO/check-second\"; exit 75",
    ]);
    assert_check_busy(&e, runner.child.id(), pgid);
    assert!(!e.repo().join("check-second").exists());

    // Let the original shell reap its descendant itself. Recovery must not
    // depend on how quickly init happens to reap orphaned background children.
    std::fs::write(e.repo().join("check-child-release"), "").unwrap();
    check_wait_until("the crashed runner's check group to finish", || {
        !check_group_alive(pgid)
    });
    let recovered = e.ok(&["check", "--all"]);
    assert_eq!(recovered["busy"], false, "{recovered:#}");
    assert_eq!(recovered["results"][0]["result"], "retry");
    assert!(e.repo().join("check-second").exists());
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
}

#[test]
fn check_shell_exit_cleans_up_surviving_descendants_before_release() {
    let e = Env::new();
    let check = STUBBORN_CHECK.replace(
        "child=$!; wait \"$child\"; exit 75",
        "while [ ! -f \"$TRK_REPO/check-release\" ]; do sleep 0.02; done; exit 75",
    );
    let id = e.waiting("background check", &check);
    let mut runner = CheckRunner::spawn(&e);
    let pgid = runner.ready();
    check_wait_until("the background descendant to start", || {
        std::fs::read_to_string(e.repo().join("check-child"))
            .is_ok_and(|s| s.trim().parse::<i64>().is_ok())
    });
    e.ok(&[
        "wait",
        &id,
        "-c",
        "echo second > \"$TRK_REPO/check-second\"; exit 75",
    ]);
    assert_check_busy(&e, runner.child.id(), pgid);
    assert!(!e.repo().join("check-second").exists());
    std::fs::write(e.repo().join("check-release"), "").unwrap();
    assert!(runner.wait().success());
    assert!(
        !check_group_alive(pgid),
        "the background check survived release"
    );
    assert_eq!(e.ok(&["show", &id])["state"], "waiting");
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
    assert_eq!(e.ok(&["check", "--all"])["results"][0]["result"], "retry");
    assert!(e.repo().join("check-second").exists());
}
