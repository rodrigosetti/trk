use super::*;
use trk_core::ops::CHECK_OUTPUT_TAIL;

fn assert_no_check_logs(e: &Env) {
    let logs = e.trk_dir().join("checks");
    assert!(
        !logs.exists() || std::fs::read_dir(logs).unwrap().next().is_none(),
        "check left a log behind"
    );
}

#[test]
fn check_large_output_keeps_a_bounded_tail_in_json_and_notes() {
    let e = Env::new();
    // Both ordinary text and invalid UTF-8 must stay bounded after decoding.
    for replacement in ["x", "\\377"] {
        let id = e.waiting(
            "chatty check",
            &format!(
                "dd if=/dev/zero bs=65536 count=128 2>/dev/null | LC_ALL=C tr '\\000' '{replacement}'; \
                 printf '\\nstdout-tail-λ\\n'; printf 'stderr-tail\\n' >&2; exit 3"
            ),
        );
        let r = e.ok(&["check", &id, "--no-gc"]);
        assert_eq!(r["results"][0]["result"], "failed");
        assert_eq!(r["results"][0]["exit"], 3);
        let output = r["results"][0]["output"].as_str().unwrap();
        assert!(output.len() <= CHECK_OUTPUT_TAIL, "{} bytes", output.len());
        assert!(output.starts_with("[trk] output truncated; showing tail\n"));
        assert!(output.ends_with("stdout-tail-λ\nstderr-tail"), "{output:?}");
        if replacement != "x" {
            assert!(output.contains('�'), "invalid bytes missing: {output:?}");
        }
        let issue = e.ok(&["show", &id]);
        let note = issue["notes"].as_array().unwrap().last().unwrap()["body"]
            .as_str()
            .unwrap();
        assert!(note.contains(output), "JSON and note output differ: {note}");
        let snapshot = e.ok(&["dashboard"]);
        let run = &snapshot["check_runs"][0];
        assert_eq!(run["issue_id"], id);
        assert_eq!(run["result"], "failed");
        assert_eq!(run["output"].as_str().unwrap().trim_end(), output);
        assert_eq!(run["output_truncated"], true);
        assert_no_check_logs(&e);
    }
}

#[test]
fn check_continuous_output_does_not_starve_timeout() {
    let e = Env::new();
    e.set_config("timeout  = \"10m\"", "timeout  = \"1s\"");
    let id = e.waiting(
        "endless output",
        "echo $$ > \"$TRK_REPO/check-shell\"; \
         while :; do printf 'continuously-writing-check-output\\n'; done",
    );
    let json = tempfile::NamedTempFile::new().unwrap();
    let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
    command
        .args(["--json", "check", &id, "--no-gc"])
        .stdout(json.reopen().unwrap())
        .stderr(Stdio::null());
    let mut runner = CheckRunner::spawn_command(&e, command);
    runner.ready();
    assert!(runner.wait().success());
    let r: Value = serde_json::from_str(&std::fs::read_to_string(json.path()).unwrap()).unwrap();
    let result = &r["data"]["results"][0];
    assert_eq!(result["timed_out"], true);
    let output = result["output"].as_str().unwrap();
    assert!(output.len() <= CHECK_OUTPUT_TAIL);
    assert!(output.contains("output truncated"));
    assert!(output.contains("continuously-writing-check-output"));
    assert!(output.contains("killed after 1s"));
    let issue = e.ok(&["show", &id]);
    let note = issue["notes"].as_array().unwrap().last().unwrap()["body"]
        .as_str()
        .unwrap();
    assert!(note.contains(output));
    assert_no_check_logs(&e);
}

#[test]
fn check_removes_only_dead_owner_logs_on_real_passes() {
    let e = Env::new();
    let logs = e.trk_dir().join("checks");
    std::fs::create_dir(&logs).unwrap();
    let mut dead = StdCommand::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let pid = dead.id();
    assert!(dead.wait().unwrap().success());
    let stale = logs.join(format!("trk-old-issue-{pid}.log"));
    let live = logs.join(format!("trk-live-{}.log", std::process::id()));
    let unrelated = logs.join("not-a-check.log");
    let unknown = logs.join("trk-invalid-0.log");
    let directory = logs.join(format!("trk-directory-{pid}.log"));
    for path in [&stale, &live, &unrelated, &unknown] {
        std::fs::write(path, "keep unless stale").unwrap();
    }
    std::fs::create_dir(&directory).unwrap();
    e.ok(&["check", "--dry-run", "--no-gc"]);
    assert!(stale.exists(), "dry run removed a file");
    // Even an empty queue cleans up logs, on every pass.
    for _ in 0..2 {
        e.ok(&["check", "--no-gc"]);
        assert!(!stale.exists());
        for path in [&live, &unrelated, &unknown] {
            assert_eq!(std::fs::read_to_string(path).unwrap(), "keep unless stale");
        }
        assert!(directory.is_dir());
        std::fs::write(&stale, "stale again").unwrap();
    }
}

#[test]
fn check_failed_group_cleanup_leaves_no_log() {
    let e = Env::new();
    let id = e.waiting(
        "cleanup failure",
        "echo $$ > \"$TRK_REPO/check-shell\"; echo captured-output; \
         while [ ! -f \"$TRK_REPO/check-release\" ]; do sleep 0.02; done",
    );
    let mut runner = CheckRunner::spawn(&e);
    let pgid = runner.ready();
    // Make reopening the store fail when Lock::finish clears the durable group.
    let offline = e.repo().join("offline-trk");
    std::fs::rename(e.trk_dir(), &offline).unwrap();
    std::fs::write(e.trk_dir(), "not a directory").unwrap();
    std::fs::write(e.repo().join("check-release"), "").unwrap();
    assert!(!runner.wait().success());
    std::fs::remove_file(e.trk_dir()).unwrap();
    std::fs::rename(offline, e.trk_dir()).unwrap();
    assert!(!check_group_alive(pgid));
    assert_eq!(e.ok(&["show", &id])["state"], "waiting");
    assert_no_check_logs(&e);
}

#[test]
fn dashboard_records_external_watch_runs_across_restarts() {
    let e = Env::new();
    let retry = e.waiting(
        "pending",
        "printf 'pending stdout\\n'; printf 'pending stderr\\n' >&2; exit 75",
    );
    let passed = e.waiting("complete", "echo passed-output");
    for count in [2, 3] {
        let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
        command
            .args(["check", "--all", "--watch", "1h", "--no-gc"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut watcher = CheckRunner::spawn_command(&e, command);
        check_wait_until("external watcher runs in the dashboard snapshot", || {
            e.ok(&["dashboard"])["check_runs"].as_array().unwrap().len() == count
        });
        watcher.signal("-TERM");
        watcher.wait();
        let snapshot = e.ok(&["dashboard"]);
        let runs = snapshot["check_runs"].as_array().unwrap();
        assert_eq!(runs.len(), count);
        let pending = runs.iter().find(|r| r["issue_id"] == retry).unwrap();
        assert_eq!(pending["result"], "retry");
        assert_eq!(pending["output"], "pending stdout\npending stderr\n");
        assert_eq!(pending["exit"], 75);
        assert!(pending["duration_ms"].is_u64());
        let complete = runs.iter().find(|r| r["issue_id"] == passed).unwrap();
        assert_eq!(complete["result"], "passed");
        assert_eq!(complete["output"], "passed-output\n");
        assert!(
            !snapshot["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["id"] == passed)
        );
    }
}
