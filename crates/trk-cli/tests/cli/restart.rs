use super::*;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;

/// Never replace Cargo's executable: each test installs its own private copy.
/// The replacement records the exec boundary, then installs and execs another
/// copy of the exact same build (and therefore the same semantic version).
struct Installation {
    binary: PathBuf,
    replacement: PathBuf,
    output: PathBuf,
}

impl Installation {
    fn new(e: &Env) -> Self {
        let binary = e.dir.path().join("installed-trk");
        let next = e.dir.path().join("next-trk");
        let replacement = e.dir.path().join("replacement");
        let cargo_binary = assert_cmd::cargo::cargo_bin("trk");
        std::fs::copy(&cargo_binary, &binary).unwrap();
        std::fs::copy(&cargo_binary, &next).unwrap();
        let script = format!(
            "#!/bin/sh\n\
             set -eu\n\
             printf '%s\\0' \"$@\" > restart-args\n\
             echo $$ > restart-pid\n\
             '{cargo}' --json whoami > restart-lock.json\n\
             test ! -f check-second\n\
             mv '{next}' '{binary}'\n\
             touch restarted\n\
             exec '{binary}' \"$@\"\n",
            cargo = cargo_binary.display(),
            next = next.display(),
            binary = binary.display(),
        );
        std::fs::write(&replacement, script).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            binary,
            replacement,
            output: e.dir.path().join("runner-output"),
        }
    }

    fn spawn(&self, e: &Env, args: &[&str]) -> CheckRunner {
        let mut command = StdCommand::new(&self.binary);
        command
            .args(args)
            // Even a preexisting non-UTF8 handoff variable must be preserved.
            .env(
                "_TRK_CHECK_RESTART",
                std::ffi::OsString::from_vec(vec![0xff, b'x']),
            )
            .stdout(std::fs::File::create(&self.output).unwrap())
            .stderr(Stdio::inherit());
        CheckRunner::spawn_command(e, command)
    }

    fn replace(&self) {
        std::fs::rename(&self.replacement, &self.binary).unwrap();
    }

    fn assert_restart(&self, e: &Env, runner: &CheckRunner, args: &[&str]) {
        let pid = std::fs::read_to_string(e.repo().join("restart-pid")).unwrap();
        assert_eq!(
            pid.trim(),
            runner.child.id().to_string(),
            "must exec, not spawn"
        );
        let argv = std::fs::read(e.repo().join("restart-args")).unwrap();
        let expected: Vec<u8> = args.iter().flat_map(|s| s.bytes().chain([0])).collect();
        assert_eq!(argv, expected);
        let lock: Value =
            serde_json::from_slice(&std::fs::read(e.repo().join("restart-lock.json")).unwrap())
                .unwrap();
        assert_eq!(lock["data"]["check_running"], Value::Null);
    }
}

fn active_restart(exit: i32, targeted: bool) {
    let e = Env::new();
    let command = STUBBORN_CHECK.replace(
        "child=$!; wait \"$child\"; exit 75",
        &format!(
            "echo \"$TRK_ISSUE\" >> \"$TRK_REPO/executions\"; \
             while [ ! -f \"$TRK_REPO/check-release\" ]; do sleep 0.02; done; \
             echo active-output; exit {exit}"
        ),
    );
    let first = e.waiting("installs a new trk", &command);
    e.ok(&["edit", &first, "-p", "P0"]);
    let second = e.waiting(
        "must use the new executable",
        "test -f \"$TRK_REPO/restarted\" || exit 42; \
         echo \"$TRK_ISSUE\" >> \"$TRK_REPO/executions\"; \
         printf '%s' \"$_TRK_CHECK_RESTART\" > \"$TRK_REPO/restart-env\"; \
         touch \"$TRK_REPO/check-second\"",
    );
    let install = Installation::new(&e);
    let args = [
        "--json",
        "--as",
        "human:restart@example.com",
        "check",
        if targeted { &first } else { "--all" },
    ];
    let mut runner = install.spawn(&e, &args);
    let pgid = runner.ready();
    check_wait_until("the TERM-resistant descendant", || {
        e.repo().join("check-child").exists()
    });
    install.replace();
    assert_check_busy(&e, runner.child.id(), pgid);
    assert!(
        !e.repo().join("restarted").exists(),
        "restarted during a check"
    );
    std::fs::write(e.repo().join("check-release"), "").unwrap();
    assert!(runner.wait().success());
    assert!(!check_group_alive(pgid), "descendant survived re-exec");
    install.assert_restart(&e, &runner, &args);

    let response: Value = serde_json::from_slice(&std::fs::read(&install.output).unwrap()).unwrap();
    assert_eq!(response["ok"], true, "{response:#}");
    let results = response["data"]["results"].as_array().unwrap();
    assert_eq!(results.len(), if targeted { 1 } else { 2 });
    assert_eq!(results[0]["issue"], first);
    assert_eq!(results[0]["exit"], exit);
    assert_eq!(
        results[0]["result"],
        match exit {
            0 => "passed",
            75 => "retry",
            _ => "failed",
        }
    );
    if exit == 3 {
        assert!(
            results[0]["output"]
                .as_str()
                .unwrap()
                .contains("active-output")
        );
        assert_eq!(e.ok(&["show", &first])["state"], "open");
    } else if exit == 75 {
        let issue = e.ok(&["show", &first]);
        assert_eq!(issue["waiting"]["last_check_exit"], 75);
        assert_eq!(issue["waiting"]["next_check_at"], "2026-11-20T10:05:00Z");
    } else {
        assert_eq!(e.ok(&["show", &first])["state"], "closed");
    }
    let executions = std::fs::read_to_string(e.repo().join("executions")).unwrap();
    let expected = if targeted {
        format!("{first}\n")
    } else {
        format!("{first}\n{second}\n")
    };
    assert_eq!(executions, expected, "completed retry must not run again");
    if targeted {
        assert_eq!(e.ok(&["show", &second])["state"], "waiting");
    } else {
        assert_eq!(results[1]["issue"], second);
        assert_eq!(results[1]["result"], "passed");
        assert_eq!(
            std::fs::read(e.repo().join("restart-env")).unwrap(),
            [0xff, b'x']
        );
    }
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
}

#[test]
fn restart_between_queued_issues_preserves_pass_retry_and_failure() {
    for exit in [0, 75, 3] {
        active_restart(exit, false);
    }
}

#[test]
fn restart_of_explicit_id_does_not_replay_or_reject_completed_issue() {
    for exit in [0, 75, 3] {
        active_restart(exit, true);
    }
}

#[test]
fn restart_while_watch_is_idle_preserves_emitted_results() {
    let e = Env::new();
    let id = e.waiting(
        "idle watcher",
        "echo $$ > \"$TRK_REPO/check-shell\"; exit 75",
    );
    let install = Installation::new(&e);
    let args = ["check", "--all", "--watch", "1h"];
    let mut runner = install.spawn(&e, &args);
    runner.ready();
    check_wait_until("the watcher to print its completed result", || {
        std::fs::read_to_string(&install.output).is_ok_and(|s| s.contains("not yet"))
    });
    let next = e.waiting(
        "queued while idle",
        "test -f \"$TRK_REPO/restarted\" || exit 42; \
         printf '%s' \"$_TRK_CHECK_RESTART\" > \"$TRK_REPO/restart-env\"; exit 75",
    );
    install.replace();
    check_wait_until("idle watcher restart without waiting one hour", || {
        std::fs::read_to_string(&install.output).is_ok_and(|s| s.contains(&next))
    });
    install.assert_restart(&e, &runner, &args);
    runner.signal("-INT");
    assert!(!runner.wait().success());
    let output = std::fs::read_to_string(&install.output).unwrap();
    assert_eq!(output.matches(&id).count(), 1, "{output}");
    assert_eq!(output.matches(&next).count(), 1, "{output}");
    assert_eq!(
        std::fs::read(e.repo().join("restart-env")).unwrap(),
        [0xff, b'x']
    );
    assert_eq!(e.ok(&["show", &id])["waiting"]["last_check_exit"], 75);
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
}
