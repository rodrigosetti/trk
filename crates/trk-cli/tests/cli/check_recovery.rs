use super::*;

fn seed_check_holder(e: &Env, pid: i64, host: Option<&str>, started: &str, pgid: Option<i64>) {
    let store =
        trk_core::Store::open(&e.trk_dir().join("trk.db"), trk_core::StoreOpts::default()).unwrap();
    store.conn().execute(
        "INSERT OR REPLACE INTO check_lock (id, pid, host, started_at, pgid) VALUES (1, ?1, ?2, ?3, ?4)",
        (pid, host, started, pgid),
    ).unwrap();
}

#[test]
fn check_recovers_a_recycled_pid_without_touching_the_unrelated_process() {
    let e = Env::new();
    let id = e.waiting(
        "recycled pid",
        "echo ran > \"$TRK_REPO/recovered\"; exit 75",
    );
    // This live test process cannot be the runner from the year 2000.
    seed_check_holder(
        &e,
        i64::from(std::process::id()),
        Some(&local_host()),
        "2000-01-01T00:00:00Z",
        None,
    );
    // Previews use the same identity rules without mutating the lock.
    e.ok(&["gc", "--dry-run"]);
    assert_ne!(e.ok(&["whoami"])["check_running"], Value::Null);
    let recovered = e.ok(&["check", &id]);
    assert_eq!(recovered["busy"], false);
    assert_eq!(recovered["results"][0]["result"], "retry");
    assert_eq!(
        std::fs::read_to_string(e.repo().join("recovered")).unwrap(),
        "ran\n"
    );
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
}

#[test]
fn check_keeps_ambiguous_process_identity_and_live_groups_locked() {
    let e = Env::new();
    let id = e.waiting("protected", "touch \"$TRK_REPO/unsafe\"; exit 75");
    let pid = i64::from(std::process::id());
    let output = StdCommand::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pgid="])
        .output()
        .unwrap();
    assert!(output.status.success());
    let pgid = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse::<i64>()
        .unwrap();
    for (started, group) in [
        ("invalid timestamp", None),
        ("9999-01-01T00:00:00Z", None),
        ("2000-01-01T00:00:00Z", Some(pgid)),
    ] {
        seed_check_holder(&e, pid, Some(&local_host()), started, group);
        assert_eq!(e.ok(&["check", &id])["busy"], true);
        e.err(&["check", "--unlock"], 4, "invalid_state");
        assert!(!e.repo().join("unsafe").exists());
    }
}

#[test]
fn check_unlock_is_human_only_audited_and_recovers_unknown_hosts() {
    let e = Env::new();
    let id = e.waiting(
        "manual recovery",
        "echo ran > \"$TRK_REPO/unlocked\"; exit 75",
    );
    let here = local_host();
    for host in [Some("previous-host.invalid"), None, Some(here.as_str())] {
        // A local absent PID is safe; remote/unknown PIDs cannot be probed.
        let pid = if host == Some(here.as_str()) {
            i64::from(i32::MAX)
        } else {
            i64::from(std::process::id())
        };
        seed_check_holder(&e, pid, host, "2000-01-01T00:00:00Z", None);
        let before = e.ok(&["log"]);
        let holder = e.ok(&["whoami"])["check_running"].clone();
        e.err(
            &["check", "--unlock", "--as", "agent:omp:unlock"],
            5,
            "not_permitted",
        );
        assert_eq!(e.ok(&["whoami"])["check_running"], holder);
        assert_eq!(e.ok(&["log"]), before);
        if host != Some(here.as_str()) {
            assert_eq!(e.ok(&["check", &id])["busy"], true);
            assert!(!e.repo().join("unlocked").exists());
        }
        let unlocked = e.ok(&["check", "--unlock"]);
        assert_eq!(unlocked["unlocked"], true);
        assert_eq!(unlocked["holder"], holder);
        assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
        let log = e.ok(&["log"]);
        let event = log.as_array().unwrap().last().unwrap();
        assert_eq!(event["actor"], "human:alice@example.com");
        assert_eq!(event["issue_id"], Value::Null);
        assert_eq!(event["payload"]["check_unlock"], holder);
    }
    let before = e.ok(&["log"]);
    assert_eq!(e.ok(&["check", "--unlock"])["unlocked"], false);
    assert_eq!(e.ok(&["log"]), before);
    assert_eq!(e.ok(&["check", &id])["results"][0]["result"], "retry");
    assert!(e.repo().join("unlocked").exists());
}

fn recovery_watcher(e: &Env) -> CheckRunner {
    let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
    command
        .args(["check", "--all", "--no-gc", "--watch", "1s"])
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(e.repo().join("watch-errors")).unwrap());
    CheckRunner::spawn_command(e, command)
}

fn recovery_wait_until(description: &str, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn watcher_recovers_its_own_abandoned_lock_after_the_group_is_gone() {
    for pgid in [None, Some(i64::from(i32::MAX))] {
        let e = Env::new();
        let mut watcher = recovery_watcher(&e);
        super::check_watch::snapshot(&e, watcher.child.id(), false);
        watcher.signal("-STOP");
        let id = e.waiting("self recovery", "touch \"$TRK_REPO/recovered\"");
        seed_check_holder(
            &e,
            i64::from(watcher.child.id()),
            Some(&local_host()),
            "9999-01-01T00:00:00Z",
            pgid,
        );
        watcher.signal("-CONT");
        check_wait_until("the recovered check to close", || {
            e.ok(&["show", &id])["state"] == "closed"
        });
        assert!(e.repo().join("recovered").exists());
        watcher.signal("-TERM");
        assert!(!watcher.wait().success());
        assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
    }
}

#[test]
fn watcher_exits_instead_of_bypassing_its_own_live_check_group() {
    let e = Env::new();
    let mut watcher = recovery_watcher(&e);
    super::check_watch::snapshot(&e, watcher.child.id(), false);
    watcher.signal("-STOP");
    e.waiting("protected self lock", "touch \"$TRK_REPO/unsafe\"");
    // The watcher has its own process group, which remains live while it runs.
    let pid = i64::from(watcher.child.id());
    seed_check_holder(
        &e,
        pid,
        Some(&local_host()),
        "9999-01-01T00:00:00Z",
        Some(pid),
    );
    watcher.signal("-CONT");
    assert!(!watcher.wait().success());
    assert!(!e.repo().join("unsafe").exists());
    assert_eq!(e.ok(&["whoami"])["check_running"]["pid"], pid);
}

fn watcher_release_contention(permanent: bool) {
    let e = Env::new();
    let id = e.waiting(
        "release contention",
        "echo $$ > \"$TRK_REPO/check-shell\"; \
         if [ -f \"$TRK_REPO/attempted\" ]; then touch \"$TRK_REPO/recovered\"; exit 0; fi; \
         touch \"$TRK_REPO/attempted\"; \
         while [ ! -f \"$TRK_REPO/release-check\" ]; do sleep 0.02; done; exit 75",
    );
    let mut watcher = recovery_watcher(&e);
    watcher.ready();
    let store =
        trk_core::Store::open(&e.trk_dir().join("trk.db"), trk_core::StoreOpts::default()).unwrap();
    store.conn().execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(e.repo().join("release-check"), "").unwrap();
    // Keep the writer until both normal finish and a cleanup attempt have hit
    // the real SQLite busy timeout, rather than relying on a timed sleep.
    recovery_wait_until("a failed queue cleanup attempt", || {
        std::fs::read_to_string(e.repo().join("watch-errors"))
            .unwrap()
            .contains("retrying check queue cleanup")
    });
    if permanent {
        let mut status = None;
        recovery_wait_until("the watcher to exit after bounded cleanup retries", || {
            status = watcher.child.try_wait().unwrap();
            status.is_some()
        });
        assert!(!status.unwrap().success());
        store.conn().execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            e.ok(&["whoami"])["check_running"]["pid"],
            watcher.child.id()
        );
        // A replacement runner recovers the dead owner's row without --unlock.
        assert_eq!(e.ok(&["check", &id])["results"][0]["result"], "passed");
    } else {
        store.conn().execute_batch("ROLLBACK").unwrap();
        check_wait_until("a later watcher pass to run its check", || {
            e.ok(&["show", &id])["state"] == "closed"
        });
        assert!(watcher.child.try_wait().unwrap().is_none());
        watcher.signal("-TERM");
        assert!(!watcher.wait().success());
    }
    assert!(e.repo().join("recovered").exists());
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
}

#[test]
fn watcher_runs_a_later_pass_after_database_contention_during_release() {
    watcher_release_contention(false);
}

#[test]
fn watcher_exits_after_persistent_database_contention_during_release() {
    watcher_release_contention(true);
}
